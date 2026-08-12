//! OwO Agent SDK HTTP 服务（M1 + v0.4）：session/turn/permission/diff/revert/abort + SSE，
//! 以及 v0.4 接口：context.snapshot / perception.subscribe / learn.* / skill.verify /
//! proactive.suggest / whitelist.manage。

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use owo_agent_core::learn::{
    LearnRecorder, LearnState, ProactiveEngine, ProactiveSuggestion, RecordedAction,
    SuggestionAction,
};
use owo_agent_core::perception::{SituationSnapshot, SituationStore};
use owo_agent_core::permissions::{Approver, Decision, PermissionRequest};
use owo_agent_core::session::{Session, SessionStore};
use owo_agent_core::validate_skill_package;
use owo_agent_core::whitelist::{Whitelist, WhitelistEntry};
use owo_agent_core::Agent;
use owo_agent_protocol::{
    CreateSessionRequest, EvalRunRequest, FileDiff, ForkRequest, HealthResponse,
    PermissionResponse, RewindRequest, SessionInfo, SseEvent, TurnRequest,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

pub struct AppState {
    pub agent: Arc<Agent>,
    pub store: Arc<dyn SessionStore>,
    pub sessions: Arc<Mutex<HashMap<String, Session>>>,
    pub pending_approvals: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<Decision>>>>,
    pub aborts: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    pub traces_dir: PathBuf,
    pub perception: Arc<Mutex<SituationStore>>,
    pub whitelist: Arc<Mutex<Whitelist>>,
    pub learn: Arc<Mutex<LearnRecorder>>,
    pub proactive: Arc<Mutex<ProactiveEngine>>,
}

impl AppState {
    pub fn new(agent: Agent, store: impl SessionStore + 'static, traces_dir: PathBuf) -> Self {
        Self {
            agent: Arc::new(agent),
            store: Arc::new(store),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            pending_approvals: Arc::new(Mutex::new(HashMap::new())),
            aborts: Arc::new(Mutex::new(HashMap::new())),
            traces_dir,
            perception: Arc::new(Mutex::new(SituationStore::new())),
            whitelist: Arc::new(Mutex::new(Whitelist::default())),
            learn: Arc::new(Mutex::new(LearnRecorder::new())),
            proactive: Arc::new(Mutex::new(ProactiveEngine::new(Default::default()))),
        }
    }
}

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/openapi.json", get(openapi_spec))
        .route("/session", post(create_session))
        .route("/session/{id}/turn", post(turn))
        .route(
            "/session/{id}/permission/{request_id}",
            post(respond_permission),
        )
        .route("/session/{id}/abort", post(abort_turn))
        .route("/session/{id}/diff", get(diff))
        .route("/session/{id}/revert", post(revert))
        .route("/session/{id}/fork", post(fork_session))
        .route("/session/{id}/rewind", post(rewind_session))
        .route("/session/{id}/redo", post(redo_session))
        .route("/session/{id}/children", get(children))
        .route("/session/{id}/export/{format}", get(export_session))
        .route("/sessions", get(list_sessions))
        .route("/skills", get(list_skills))
        .route("/eval/run", post(run_eval))
        .route("/context/snapshot", get(context_snapshot))
        .route("/perception/events", get(perception_events))
        .route("/perception/capture", post(perception_capture))
        .route("/perception/layers", post(perception_layers))
        .route("/learn/record", post(learn_record))
        .route("/learn/pause", post(learn_pause))
        .route("/learn/resume", post(learn_resume))
        .route("/learn/clear", post(learn_clear))
        .route("/learn/status", get(learn_status))
        .route("/skill/verify", post(skill_verify))
        .route("/proactive/observe", post(proactive_observe))
        .route("/proactive/decide", post(proactive_decide))
        .route("/whitelist", get(whitelist_list))
        .route("/whitelist/manage", post(whitelist_manage))
        .fallback_service(ServeDir::new(desktop_web_dir()))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// 开发环境下的桌面工作台静态目录：`<repo>/agent-sdk/desktop/web`。
fn desktop_web_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|parent| parent.parent())
        .map(|root| root.join("desktop").join("web"))
        .unwrap_or_else(|| PathBuf::from("desktop/web"))
}

async fn openapi_spec() -> Json<Value> {
    Json(serde_json::json!({
        "openapi": "3.1.0",
        "info": { "title": "OwO Agent SDK API", "version": env!("CARGO_PKG_VERSION") },
        "servers": [{ "url": "http://127.0.0.1:4096" }],
        "paths": {
            "/health": { "get": { "operationId": "health", "responses": { "200": { "description": "ok" } } } },
            "/session": { "post": {
                "operationId": "createSession",
                "requestBody": { "content": { "application/json": { "schema": { "$ref": "#/components/schemas/CreateSessionRequest" } } } },
                "responses": { "200": { "description": "session created", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SessionInfo" } } } } }
            } },
            "/session/{id}/turn": { "post": {
                "operationId": "agentTurn",
                "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }],
                "requestBody": { "content": { "application/json": { "schema": { "$ref": "#/components/schemas/TurnRequest" } } } },
                "responses": { "200": { "description": "SSE event stream" } }
            } },
            "/session/{id}/abort": { "post": { "operationId": "abortTurn", "parameters": [path_param("id")], "responses": { "200": { "description": "ok" } } } },
            "/session/{id}/permission/{request_id}": { "post": { "operationId": "respondPermission", "parameters": [path_param("id"), path_param("request_id")], "responses": { "200": { "description": "ok" } } } },
            "/session/{id}/diff": { "get": { "operationId": "sessionDiff", "parameters": [path_param("id")], "responses": { "200": { "description": "diff list" } } } },
            "/session/{id}/revert": { "post": { "operationId": "sessionRevert", "parameters": [path_param("id")], "responses": { "200": { "description": "ok" } } } },
            "/session/{id}/fork": { "post": { "operationId": "sessionFork", "parameters": [path_param("id")], "responses": { "200": { "description": "forked session" } } } },
            "/session/{id}/rewind": { "post": { "operationId": "sessionRewind", "parameters": [path_param("id")], "responses": { "200": { "description": "ok" } } } },
            "/session/{id}/redo": { "post": { "operationId": "sessionRedo", "parameters": [path_param("id")], "responses": { "200": { "description": "ok" } } } },
            "/session/{id}/children": { "get": { "operationId": "sessionChildren", "parameters": [path_param("id")], "responses": { "200": { "description": "children" } } } },
            "/session/{id}/export/{format}": { "get": { "operationId": "exportSession", "parameters": [path_param("id"), path_param("format")], "responses": { "200": { "description": "md or html" } } } },
            "/sessions": { "get": { "operationId": "listSessions", "responses": { "200": { "description": "session list" } } } },
            "/skills": { "get": { "operationId": "listSkills", "responses": { "200": { "description": "skill list" } } } },
            "/eval/run": { "post": { "operationId": "runEval", "requestBody": { "content": { "application/json": { "schema": { "$ref": "#/components/schemas/EvalRunRequest" } } } }, "responses": { "200": { "description": "eval report" } } } },
            "/context/snapshot": { "get": { "operationId": "contextSnapshot", "responses": { "200": { "description": "situation snapshot" } } } },
            "/perception/events": { "get": { "operationId": "perceptionSubscribe", "responses": { "200": { "description": "SSE perception event stream" } } } },
            "/perception/capture": { "post": { "operationId": "perceptionCapture", "responses": { "200": { "description": "capture meta with OCR summary" } } } },
            "/perception/layers": { "post": { "operationId": "perceptionLayers", "responses": { "200": { "description": "layer authorization updated" } } } },
            "/learn/record": { "post": { "operationId": "learnRecord", "responses": { "200": { "description": "learn state" } } } },
            "/learn/pause": { "post": { "operationId": "learnPause", "responses": { "200": { "description": "learn state" } } } },
            "/learn/resume": { "post": { "operationId": "learnResume", "responses": { "200": { "description": "learn state" } } } },
            "/learn/clear": { "post": { "operationId": "learnClear", "responses": { "200": { "description": "ok" } } } },
            "/skill/verify": { "post": { "operationId": "skillVerify", "responses": { "200": { "description": "validation result" } } } },
            "/proactive/observe": { "post": { "operationId": "proactiveObserve", "responses": { "200": { "description": "optional suggestion" } } } },
            "/proactive/decide": { "post": { "operationId": "proactiveDecide", "responses": { "200": { "description": "ok" } } } },
            "/whitelist": { "get": { "operationId": "whitelistList", "responses": { "200": { "description": "whitelist entries" } } } },
            "/whitelist/manage": { "post": { "operationId": "whitelistManage", "responses": { "200": { "description": "whitelist entries" } } } }
        },
        "components": {
            "schemas": {
                "CreateSessionRequest": {
                    "type": "object",
                    "properties": {
                        "workspace": { "type": "string" },
                        "model": { "type": "string" },
                        "system_prompt": { "type": "string" }
                    },
                    "required": ["workspace"]
                },
                "SessionInfo": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "workspace": { "type": "string" },
                        "model": { "type": "string" },
                        "created_at": { "type": "string" }
                    }
                },
                "TurnRequest": {
                    "type": "object",
                    "properties": { "prompt": { "type": "string" } },
                    "required": ["prompt"]
                },
                "EvalRunRequest": {
                    "type": "object",
                    "properties": { "suite_id": { "type": "string" } },
                    "required": ["suite_id"]
                }
            }
        }
    }))
}

fn path_param(name: &str) -> Value {
    serde_json::json!({ "name": name, "in": "path", "required": true, "schema": { "type": "string" } })
}

fn to_session_info(session: &Session) -> SessionInfo {
    SessionInfo {
        id: session.id.clone(),
        workspace: session.workspace.to_string_lossy().into_owned(),
        model: session.model.clone(),
        created_at: session.created_at.clone(),
    }
}

fn load_session(state: &AppState, id: &str) -> Result<Session, (StatusCode, String)> {
    if let Ok(sessions) = state.sessions.lock() {
        if let Some(session) = sessions.get(id) {
            return Ok(session.clone());
        }
    }
    state.store.load(id).map_err(|error| {
        (
            StatusCode::NOT_FOUND,
            format!("会话不存在：{id}（{error}）"),
        )
    })
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        healthy: true,
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

async fn list_sessions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<SessionInfo>>, (StatusCode, String)> {
    let mut sessions = Vec::new();
    for session_id in state.store.list() {
        if let Ok(session) = state.store.load(&session_id) {
            sessions.push(to_session_info(&session));
        }
    }
    sessions.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    Ok(Json(sessions))
}

async fn list_skills(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<Value>>, (StatusCode, String)> {
    let skills = state.agent.skills().list();
    Ok(Json(
        skills
            .iter()
            .map(|skill| {
                json!({
                    "name": skill.name,
                    "description": skill.description,
                    "path": skill.path.to_string_lossy(),
                })
            })
            .collect(),
    ))
}

async fn create_session(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateSessionRequest>,
) -> Result<Json<SessionInfo>, (StatusCode, String)> {
    let workspace = std::path::PathBuf::from(&request.workspace);
    if !workspace.is_dir() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("工作区不存在：{}", request.workspace),
        ));
    }
    let model = request.model.unwrap_or_else(|| {
        std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".to_string())
    });
    let session = state
        .store
        .create(&workspace, &model, request.system_prompt.as_deref())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    state
        .sessions
        .lock()
        .map_err(poison)?
        .insert(session.id.clone(), session.clone());
    Ok(Json(SessionInfo {
        id: session.id,
        workspace: request.workspace,
        model: session.model,
        created_at: session.created_at,
    }))
}

async fn turn(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<TurnRequest>,
) -> Result<Sse<ReceiverStream<Result<Event, Infallible>>>, (StatusCode, String)> {
    let session = load_session(&state, &id)?;

    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(128);
    let approver = ChannelApprover {
        pending: Arc::clone(&state.pending_approvals),
        event_tx: tx.clone(),
    };
    let abort_flag = {
        let mut aborts = state.aborts.lock().map_err(poison)?;
        aborts
            .entry(id.clone())
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone()
    };

    let agent = Arc::clone(&state.agent);
    let store = Arc::clone(&state.store);
    let sessions = Arc::clone(&state.sessions);
    let traces_dir = state.traces_dir.clone();
    tokio::spawn(async move {
        let mut current = session;
        let mut on_event = |event: &owo_agent_core::TurnEvent| {
            if let Some(sse) = to_sse(event) {
                let _ = tx.try_send(to_event(sse));
            }
        };
        match agent
            .run_turn(
                &mut current,
                &request.prompt,
                &approver,
                &abort_flag,
                &mut on_event,
            )
            .await
        {
            Ok(outcome) => {
                let trace = owo_agent_core::TraceRecord::from_outcome(&current, &outcome);
                let _ = owo_agent_core::save_trace(&traces_dir, &trace);
            }
            Err(error) => {
                let _ = tx.try_send(to_event(SseEvent::Progress {
                    message: format!("turn failed: {error}"),
                }));
            }
        }
        if let Ok(mut sessions) = sessions.lock() {
            sessions.insert(current.id.clone(), current.clone());
        }
        if let Err(error) = store.save(&current) {
            let _ = tx.try_send(to_event(SseEvent::Progress {
                message: format!("session save failed: {error}"),
            }));
        }
    });

    Ok(Sse::new(ReceiverStream::new(rx)))
}

async fn respond_permission(
    State(state): State<Arc<AppState>>,
    AxumPath((_session_id, request_id)): AxumPath<(String, String)>,
    Json(response): Json<PermissionResponse>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let sender = state
        .pending_approvals
        .lock()
        .map_err(poison)?
        .remove(&request_id)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("审批请求不存在：{request_id}"),
            )
        })?;
    let decision = if response.allow {
        Decision::Allow
    } else {
        Decision::Deny
    };
    sender
        .send(decision)
        .map_err(|_| (StatusCode::GONE, "审批通道已关闭".to_string()))?;
    Ok(Json(json!({ "ok": true })))
}

async fn abort_turn(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let flag = {
        let mut aborts = state.aborts.lock().map_err(poison)?;
        aborts
            .entry(id)
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone()
    };
    flag.store(true, Ordering::Relaxed);
    Ok(Json(json!({ "ok": true })))
}

async fn diff(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Vec<FileDiff>>, (StatusCode, String)> {
    let session = load_session(&state, &id)?;
    Ok(Json(session.diff()))
}

async fn revert(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut session = load_session(&state, &id)?;
    let restored = session
        .revert()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("回滚失败：{e}")))?;
    state
        .store
        .save(&session)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    state
        .sessions
        .lock()
        .map_err(poison)?
        .insert(session.id.clone(), session);
    Ok(Json(json!({ "ok": true, "restored": restored })))
}

async fn fork_session(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<ForkRequest>,
) -> Result<Json<SessionInfo>, (StatusCode, String)> {
    let session = load_session(&state, &id)?;
    let child = session.fork(request.message_index);
    state
        .store
        .save(&child)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    state
        .sessions
        .lock()
        .map_err(poison)?
        .insert(child.id.clone(), child.clone());
    Ok(Json(to_session_info(&child)))
}

async fn rewind_session(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<RewindRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut session = load_session(&state, &id)?;
    let removed = session.rewind(request.keep);
    state
        .store
        .save(&session)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    state
        .sessions
        .lock()
        .map_err(poison)?
        .insert(session.id.clone(), session);
    Ok(Json(json!({ "ok": true, "removed": removed.len() })))
}

async fn redo_session(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut session = load_session(&state, &id)?;
    let restored = session.redo().map(|tail| tail.len()).unwrap_or(0);
    state
        .store
        .save(&session)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    state
        .sessions
        .lock()
        .map_err(poison)?
        .insert(session.id.clone(), session);
    Ok(Json(json!({ "ok": true, "restored": restored })))
}

async fn children(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Vec<SessionInfo>>, (StatusCode, String)> {
    let mut result = Vec::new();
    for session_id in state.store.list() {
        if let Ok(session) = state.store.load(&session_id) {
            if session.parent_id.as_deref() == Some(id.as_str()) {
                result.push(to_session_info(&session));
            }
        }
    }
    Ok(Json(result))
}

async fn export_session(
    State(state): State<Arc<AppState>>,
    AxumPath((id, format)): AxumPath<(String, String)>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    let session = load_session(&state, &id)?;
    let (body, content_type) = match format.as_str() {
        "md" | "markdown" => (
            owo_agent_core::export_markdown(&session),
            "text/markdown; charset=utf-8",
        ),
        "html" => (
            owo_agent_core::export_html(&session),
            "text/html; charset=utf-8",
        ),
        _ => return Err((StatusCode::BAD_REQUEST, "格式仅支持 md / html".to_string())),
    };
    Ok((
        axum::http::StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, content_type)],
        body,
    )
        .into_response())
}

async fn run_eval(
    State(state): State<Arc<AppState>>,
    Json(request): Json<EvalRunRequest>,
) -> Result<Json<owo_agent_core::EvalReport>, (StatusCode, String)> {
    let suite = match request.suite_id.as_str() {
        "builtin" | "builtin-demo" => owo_agent_core::builtin_suite(),
        _ => {
            return Err((
                StatusCode::NOT_FOUND,
                format!("未知评估套件：{}", request.suite_id),
            ))
        }
    };
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".to_string());
    let provider = state.agent.provider();
    let report = owo_agent_core::run_suite(provider, &model, &suite).await;
    Ok(Json(report))
}

// ---------- v0.4 接口 ----------

async fn context_snapshot(
    State(state): State<Arc<AppState>>,
) -> Result<Json<SituationSnapshot>, (StatusCode, String)> {
    let mut perception = state.perception.lock().map_err(poison)?;
    let _ = perception.refresh_from_platform();
    let sequence = owo_agent_core::clipboard_sequence();
    perception.refresh_clipboard(sequence);
    let _ = perception.refresh_from_uia(2, 64);
    Ok(Json(perception.snapshot()))
}

/// perception.subscribe：订阅 L0/L1 事件流（SSE），桌面端感知状态区使用。
async fn perception_events(
    State(state): State<Arc<AppState>>,
) -> Result<Sse<ReceiverStream<Result<Event, Infallible>>>, (StatusCode, String)> {
    let mut perception = state.perception.lock().map_err(poison)?;
    let _ = perception.refresh_from_platform();
    let mut receiver = perception.subscribe();
    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(128);
    tokio::spawn(async move {
        while let Some(event) = receiver.recv().await {
            let data = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
            if tx
                .send(Ok(Event::default().event("perception").data(data)))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    Ok(Sse::new(ReceiverStream::new(rx)))
}

/// L2 按需采集：截图 + 本地 OCR 摘要进内存环形缓冲（不落盘）。
async fn perception_capture(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CaptureRequest>,
) -> Result<Json<owo_agent_core::CaptureMeta>, (StatusCode, String)> {
    let mut perception = state.perception.lock().map_err(poison)?;
    let frame = match (request.width, request.height) {
        (Some(width), Some(height)) => perception
            .begin_capture_region(width, height)
            .map_err(|error| (StatusCode::BAD_REQUEST, error))?,
        _ => perception
            .begin_capture_from_screen()
            .map_err(|error| (StatusCode::BAD_REQUEST, error))?,
    };
    Ok(Json(frame))
}

#[derive(serde::Deserialize)]
struct CaptureRequest {
    #[serde(default)]
    width: Option<i32>,
    #[serde(default)]
    height: Option<i32>,
}

#[derive(serde::Deserialize)]
struct LayersRequest {
    layer: String,
    enabled: bool,
}

/// 感知层级授权开关（L0-L3 逐项授权，可热撤）。
async fn perception_layers(
    State(state): State<Arc<AppState>>,
    Json(request): Json<LayersRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    use owo_agent_core::PerceptionLayer;
    let layer = match request.layer.as_str() {
        "l0_event" => PerceptionLayer::L0Event,
        "l1_ui" => PerceptionLayer::L1Ui,
        "l2_visual" => PerceptionLayer::L2Visual,
        "l3_semantic" => PerceptionLayer::L3Semantic,
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("未知感知层：{other}（l0_event/l1_ui/l2_visual/l3_semantic）"),
            ))
        }
    };
    let mut perception = state.perception.lock().map_err(poison)?;
    perception.set_layer_enabled(layer, request.enabled);
    Ok(Json(
        json!({ "layer": request.layer, "enabled": request.enabled }),
    ))
}

#[derive(serde::Deserialize)]
struct LearnRecordRequest {
    action: RecordedAction,
}

async fn learn_record(
    State(state): State<Arc<AppState>>,
    Json(request): Json<LearnRecordRequest>,
) -> Result<Json<LearnState>, (StatusCode, String)> {
    let mut learn = state.learn.lock().map_err(poison)?;
    learn
        .record(request.action)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    Ok(Json(learn.state()))
}

async fn learn_pause(
    State(state): State<Arc<AppState>>,
) -> Result<Json<LearnState>, (StatusCode, String)> {
    let mut learn = state.learn.lock().map_err(poison)?;
    learn.pause();
    Ok(Json(learn.state()))
}

async fn learn_resume(
    State(state): State<Arc<AppState>>,
) -> Result<Json<LearnState>, (StatusCode, String)> {
    let mut learn = state.learn.lock().map_err(poison)?;
    learn.resume();
    Ok(Json(learn.state()))
}

async fn learn_clear(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut learn = state.learn.lock().map_err(poison)?;
    learn.clear();
    Ok(Json(json!({ "ok": true })))
}

async fn learn_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let learn = state.learn.lock().map_err(poison)?;
    Ok(Json(json!({
        "state": learn.state(),
        "samples": learn.samples(),
        "sensitive_break": learn.sensitive_break(),
    })))
}

#[derive(serde::Deserialize)]
struct SkillVerifyRequest {
    path: PathBuf,
}

async fn skill_verify(Json(request): Json<SkillVerifyRequest>) -> Json<Value> {
    match validate_skill_package(&request.path) {
        Ok(info) => Json(json!({
            "ok": true,
            "name": info.name,
            "permissions": info.permissions,
            "has_tests": info.has_tests,
        })),
        Err(error) => Json(json!({ "ok": false, "error": error })),
    }
}

#[derive(serde::Deserialize)]
struct ProactiveObserveRequest {
    app_id: String,
    actions: Vec<String>,
}

async fn proactive_observe(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ProactiveObserveRequest>,
) -> Result<Json<Option<ProactiveSuggestion>>, (StatusCode, String)> {
    let mut proactive = state.proactive.lock().map_err(poison)?;
    Ok(Json(proactive.observe(&request.app_id, request.actions)))
}

#[derive(serde::Deserialize)]
struct ProactiveDecideRequest {
    suggestion_id: String,
    action: SuggestionAction,
}

async fn proactive_decide(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ProactiveDecideRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut proactive = state.proactive.lock().map_err(poison)?;
    proactive
        .decide(&request.suggestion_id, request.action)
        .map_err(|error| (StatusCode::NOT_FOUND, error))?;
    Ok(Json(json!({ "ok": true })))
}

async fn whitelist_list(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<WhitelistEntry>>, (StatusCode, String)> {
    let whitelist = state.whitelist.lock().map_err(poison)?;
    Ok(Json(whitelist.entries().to_vec()))
}

#[derive(serde::Deserialize)]
struct WhitelistManageRequest {
    action: String,
    #[serde(default)]
    entry: Option<WhitelistEntry>,
    #[serde(default)]
    app_id: Option<String>,
}

async fn whitelist_manage(
    State(state): State<Arc<AppState>>,
    Json(request): Json<WhitelistManageRequest>,
) -> Result<Json<Vec<WhitelistEntry>>, (StatusCode, String)> {
    let mut whitelist = state.whitelist.lock().map_err(poison)?;
    match request.action.as_str() {
        "upsert" => {
            let entry = request
                .entry
                .ok_or((StatusCode::BAD_REQUEST, "upsert 需要 entry".to_string()))?;
            whitelist.upsert(entry);
        }
        "remove" => {
            let app_id = request
                .app_id
                .ok_or((StatusCode::BAD_REQUEST, "remove 需要 app_id".to_string()))?;
            whitelist.remove(&app_id);
        }
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("未知操作：{other}（upsert / remove）"),
            ))
        }
    }
    Ok(Json(whitelist.entries().to_vec()))
}

struct ChannelApprover {
    pending: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<Decision>>>>,
    event_tx: mpsc::Sender<Result<Event, Infallible>>,
}

impl ChannelApprover {
    fn spawn_request(
        &self,
        request: &PermissionRequest,
    ) -> tokio::sync::oneshot::Receiver<Decision> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(request.request_id.clone(), tx);
        }
        let _ = self
            .event_tx
            .try_send(to_event(SseEvent::PermissionRequest {
                request_id: request.request_id.clone(),
                tool: request.tool.clone(),
                args: request.args.clone(),
                reason: request.reason.clone(),
            }));
        rx
    }
}

#[async_trait::async_trait]
impl Approver for ChannelApprover {
    async fn decide(&self, request: &PermissionRequest) -> Decision {
        let rx = self.spawn_request(request);
        match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
            Ok(Ok(decision)) => decision,
            _ => Decision::Deny,
        }
    }
}

fn to_sse(event: &owo_agent_core::TurnEvent) -> Option<SseEvent> {
    match event {
        owo_agent_core::TurnEvent::ModelCall => Some(SseEvent::Progress {
            message: "模型调用".to_string(),
        }),
        owo_agent_core::TurnEvent::TokenDelta { delta } => Some(SseEvent::TokenDelta {
            delta: delta.clone(),
        }),
        owo_agent_core::TurnEvent::Compaction { summary } => Some(SseEvent::Compaction {
            summary: summary.clone(),
        }),
        owo_agent_core::TurnEvent::PermissionRequest(request) => {
            Some(SseEvent::PermissionRequest {
                request_id: request.request_id.clone(),
                tool: request.tool.clone(),
                args: request.args.clone(),
                reason: request.reason.clone(),
            })
        }
        owo_agent_core::TurnEvent::ToolStart { id, tool } => Some(SseEvent::ToolUse {
            id: id.clone(),
            tool: tool.clone(),
            args: Value::Null,
        }),
        owo_agent_core::TurnEvent::ToolResult {
            id,
            tool,
            ok,
            error,
        } => Some(SseEvent::ToolResult {
            id: id.clone(),
            tool: tool.clone(),
            ok: *ok,
            error: error.clone(),
        }),
        owo_agent_core::TurnEvent::Final { text } => Some(SseEvent::Final { text: text.clone() }),
    }
}

fn poison<T>(_error: std::sync::PoisonError<T>) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, "状态锁中毒".to_string())
}

fn to_event(sse: SseEvent) -> Result<Event, Infallible> {
    let name = match &sse {
        SseEvent::Progress { .. } => "progress",
        SseEvent::ToolUse { .. } => "tool_use",
        SseEvent::ToolResult { .. } => "tool_result",
        SseEvent::PermissionRequest { .. } => "permission_request",
        SseEvent::Final { .. } => "final",
        SseEvent::TokenDelta { .. } => "token_delta",
        SseEvent::Compaction { .. } => "compaction",
    };
    let data = serde_json::to_string(&sse).unwrap_or_else(|_| "{}".to_string());
    Ok(Event::default().event(name).data(data))
}
