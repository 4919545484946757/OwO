//! OwO Agent SDK HTTP 服务（M1）：session/turn/permission/diff/revert/abort + SSE。

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use owo_agent_core::permissions::{Approver, Decision, PermissionRequest};
use owo_agent_core::session::{Session, SessionStore};
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

pub struct AppState {
    pub agent: Arc<Agent>,
    pub store: Arc<dyn SessionStore>,
    pub sessions: Arc<Mutex<HashMap<String, Session>>>,
    pub pending_approvals: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<Decision>>>>,
    pub aborts: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    pub traces_dir: PathBuf,
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
        .route("/eval/run", post(run_eval))
        .with_state(state)
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
            "/eval/run": { "post": { "operationId": "runEval", "requestBody": { "content": { "application/json": { "schema": { "$ref": "#/components/schemas/EvalRunRequest" } } } }, "responses": { "200": { "description": "eval report" } } } }
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
        std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-5.1-codex".to_string())
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
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-5.1-codex".to_string());
    let provider = state.agent.provider();
    let report = owo_agent_core::run_suite(provider, &model, &suite).await;
    Ok(Json(report))
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
