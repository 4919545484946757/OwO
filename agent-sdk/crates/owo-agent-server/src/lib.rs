#![recursion_limit = "256"]

//! OwO Agent SDK HTTP 服务（M1 + v0.4）：session/turn/permission/diff/revert/abort + SSE，
//! 以及 v0.4 接口：context.snapshot / perception.subscribe / learn.* / skill.verify /
//! proactive.suggest / whitelist.manage。

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use owo_agent_core::automation::{AutomationAction, AutomationStore, AutomationTask, Schedule};
use owo_agent_core::learn::{
    ActionType, LearnPipeline, LearnState, ProactiveEngine, ProactiveSuggestion, RecordedAction,
    SemanticAnchor, Sensitivity, SuggestionAction,
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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
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
    pub pipeline: Arc<Mutex<LearnPipeline>>,
    pub proactive: Arc<Mutex<ProactiveEngine>>,
    pub stt: Arc<Mutex<owo_agent_core::LocalStt>>,
    pub automations: Arc<Mutex<AutomationStore>>,
    pub memory: Arc<Mutex<owo_agent_core::MemoryStore>>,
    pub audit_flushed: Arc<Mutex<usize>>,
    pub workspace: PathBuf,
    pub data_root: PathBuf,
}

impl AppState {
    pub fn new(
        agent: Agent,
        store: impl SessionStore + 'static,
        traces_dir: PathBuf,
        data_root: PathBuf,
        workspace: PathBuf,
    ) -> Self {
        let settings = owo_agent_core::Settings::load(&workspace);
        let mut whitelist = Whitelist::default();
        for entry in settings.whitelist.clone() {
            whitelist.upsert(entry);
        }
        Self {
            agent: Arc::new(agent),
            store: Arc::new(store),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            pending_approvals: Arc::new(Mutex::new(HashMap::new())),
            aborts: Arc::new(Mutex::new(HashMap::new())),
            traces_dir,
            perception: Arc::new(Mutex::new(SituationStore::new())),
            whitelist: Arc::new(Mutex::new(whitelist)),
            pipeline: Arc::new(Mutex::new(LearnPipeline::new(
                data_root.join("skills").join("user"),
            ))),
            proactive: Arc::new(Mutex::new(ProactiveEngine::new(settings.proactive.clone()))),
            stt: Arc::new(Mutex::new(owo_agent_core::LocalStt::new(
                &settings.stt,
                &data_root,
            ))),
            automations: Arc::new(Mutex::new(AutomationStore::new(data_root.clone()))),
            memory: Arc::new(Mutex::new(owo_agent_core::MemoryStore::new(
                data_root.join("memory.jsonl"),
            ))),
            audit_flushed: Arc::new(Mutex::new(0)),
            workspace,
            data_root,
        }
    }
}

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/audit", get(audit_list))
        .route("/openapi.json", get(openapi_spec))
        .route("/session", post(create_session))
        .route("/session/{id}", get(get_session))
        .route("/session/{id}/turn", post(turn))
        .route("/session/{id}/attachments", get(attachments_list))
        .route("/session/{id}/attachments", post(attachment_upload))
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
        .route("/session/{id}/rename", post(session_rename))
        .route("/session/{id}/archive", post(session_archive))
        .route("/session/{id}/pin", post(session_pin))
        .route("/session/{id}/children", get(children))
        .route("/session/{id}/export/{format}", get(export_session))
        .route("/sessions", get(list_sessions))
        .route("/skills", get(list_skills))
        .route("/skills/{name}", get(skill_detail).post(skill_edit))
        .route("/skills/{name}/enabled", post(skill_enabled))
        .route("/eval/run", post(run_eval))
        .route("/context/snapshot", get(context_snapshot))
        .route("/perception/events", get(perception_events))
        .route("/perception/capture", post(perception_capture))
        .route("/perception/layers", post(perception_layers))
        .route("/perception/tree", post(perception_tree))
        .route(
            "/perception/template/build",
            post(perception_template_build),
        )
        .route(
            "/perception/template/build-ocr",
            post(perception_template_build_ocr),
        )
        .route(
            "/perception/template/detect",
            post(perception_template_detect),
        )
        .route(
            "/perception/template/detect-ocr",
            post(perception_template_detect_ocr),
        )
        .route(
            "/perception/template/{app_id}",
            get(perception_template_get),
        )
        .route("/perception/ocr", post(perception_ocr))
        .route("/perception/ocr/bytes", post(perception_ocr_bytes))
        .route("/perception/ocr/status", get(ocr_status))
        .route("/perception/ocr/region", post(perception_ocr_region))
        .route("/perception/window", post(perception_window))
        .route("/desktop/foreground", get(desktop_foreground))
        .route("/desktop/windows", get(desktop_windows))
        .route("/desktop/activate", post(desktop_activate))
        .route("/desktop/click", post(desktop_click))
        .route("/desktop/type", post(desktop_type))
        .route("/desktop/key", post(desktop_key))
        .route("/desktop/shortcut", post(desktop_shortcut))
        .route("/desktop/launch", post(desktop_launch))
        .route("/desktop/scroll", post(desktop_scroll))
        .route("/desktop/wait", post(desktop_wait))
        .route("/vision/status", get(vision_status))
        .route("/vision/describe", post(vision_describe))
        .route("/vision/verify", post(vision_verify))
        .route("/vision/ground", post(vision_ground))
        .route("/memory/observations", get(memory_observations))
        .route("/memory/clear", post(memory_clear))
        .route("/memory/mine-skill", post(memory_mine_skill))
        .route("/learn/start", post(learn_start))
        .route("/learn/record", post(learn_record))
        .route("/learn/pause", post(learn_pause))
        .route("/learn/resume", post(learn_resume))
        .route("/learn/stop", post(learn_stop))
        .route("/learn/clear", post(learn_clear))
        .route("/learn/status", get(learn_status))
        .route("/learn/execute", post(learn_execute))
        .route("/learn/packages", get(learn_packages))
        .route(
            "/learn/packages/{name}",
            get(learn_package_detail).delete(learn_package_delete),
        )
        .route("/learn/sink", post(learn_sink))
        .route("/learn/execute-package", post(learn_execute_package))
        .route("/learn/export/{name}", get(learn_export))
        .route("/learn/import", post(learn_import))
        .route("/skill/verify", post(skill_verify))
        .route("/proactive/observe", post(proactive_observe))
        .route("/proactive/decide", post(proactive_decide))
        .route("/proactive/suggestions", get(proactive_suggestions))
        .route("/stt/transcribe", post(stt_transcribe))
        .route("/automations", get(automations_list))
        .route("/automations", post(automations_create))
        .route("/automations/{id}/toggle", post(automations_toggle))
        .route(
            "/automations/{id}",
            axum::routing::delete(automations_delete),
        )
        .route("/automations/reminders", get(automations_reminders))
        .route(
            "/automations/reminders/clear",
            post(automations_clear_reminders),
        )
        .route("/settings", get(settings_get).post(settings_update))
        .route("/settings/egress", post(settings_egress))
        .route("/whitelist", get(whitelist_list))
        .route("/whitelist/manage", post(whitelist_manage))
        .fallback_service(ServeDir::new(desktop_web_dir()))
        .layer(CorsLayer::permissive())
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
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
            "/audit": { "get": { "operationId": "auditList", "parameters": [{ "name": "limit", "in": "query", "required": false, "schema": { "type": "integer" } }], "responses": { "200": { "description": "recent audit entries" } } } },
            "/session": { "post": {
                "operationId": "createSession",
                "requestBody": { "content": { "application/json": { "schema": { "$ref": "#/components/schemas/CreateSessionRequest" } } } },
                "responses": { "200": { "description": "session created", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SessionInfo" } } } } }
            } },
            "/session/{id}": { "get": { "operationId": "getSession", "parameters": [path_param("id")], "responses": { "200": { "description": "session detail with messages" } } } },
            "/session/{id}/turn": { "post": {
                "operationId": "agentTurn",
                "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }],
                "requestBody": { "content": { "application/json": { "schema": { "$ref": "#/components/schemas/TurnRequest" } } } },
                "responses": { "200": { "description": "SSE event stream" } }
            } },
            "/session/{id}/attachments": { "get": { "operationId": "attachmentsList", "parameters": [path_param("id")], "responses": { "200": { "description": "attachment list" } } }, "post": { "operationId": "attachmentUpload", "parameters": [path_param("id")], "responses": { "200": { "description": "uploaded attachment" } } } },
            "/session/{id}/abort": { "post": { "operationId": "abortTurn", "parameters": [path_param("id")], "responses": { "200": { "description": "ok" } } } },
            "/session/{id}/permission/{request_id}": { "post": { "operationId": "respondPermission", "parameters": [path_param("id"), path_param("request_id")], "responses": { "200": { "description": "ok" } } } },
            "/session/{id}/diff": { "get": { "operationId": "sessionDiff", "parameters": [path_param("id")], "responses": { "200": { "description": "diff list" } } } },
            "/session/{id}/revert": { "post": { "operationId": "sessionRevert", "parameters": [path_param("id")], "responses": { "200": { "description": "ok" } } } },
            "/session/{id}/fork": { "post": { "operationId": "sessionFork", "parameters": [path_param("id")], "responses": { "200": { "description": "forked session" } } } },
            "/session/{id}/rewind": { "post": { "operationId": "sessionRewind", "parameters": [path_param("id")], "responses": { "200": { "description": "ok" } } } },
            "/session/{id}/redo": { "post": { "operationId": "sessionRedo", "parameters": [path_param("id")], "responses": { "200": { "description": "ok" } } } },
            "/session/{id}/rename": { "post": { "operationId": "sessionRename", "parameters": [path_param("id")], "responses": { "200": { "description": "renamed session" } } } },
            "/session/{id}/archive": { "post": { "operationId": "sessionArchive", "parameters": [path_param("id")], "responses": { "200": { "description": "archive state" } } } },
            "/session/{id}/pin": { "post": { "operationId": "sessionPin", "parameters": [path_param("id")], "responses": { "200": { "description": "pin state" } } } },
            "/session/{id}/children": { "get": { "operationId": "sessionChildren", "parameters": [path_param("id")], "responses": { "200": { "description": "children" } } } },
            "/session/{id}/export/{format}": { "get": { "operationId": "exportSession", "parameters": [path_param("id"), path_param("format")], "responses": { "200": { "description": "md or html" } } } },
            "/sessions": { "get": { "operationId": "listSessions", "responses": { "200": { "description": "session list" } } } },
            "/skills": { "get": { "operationId": "listSkills", "responses": { "200": { "description": "skill list" } } } },
            "/skills/{name}": { "get": { "operationId": "skillDetail", "parameters": [path_param("name")], "responses": { "200": { "description": "skill detail with SKILL.md content" } } }, "post": { "operationId": "skillEdit", "parameters": [path_param("name")], "responses": { "200": { "description": "updated" } } } },
            "/skills/{name}/enabled": { "post": { "operationId": "skillEnabled", "parameters": [path_param("name")], "responses": { "200": { "description": "enabled state" } } } },
            "/eval/run": { "post": { "operationId": "runEval", "requestBody": { "content": { "application/json": { "schema": { "$ref": "#/components/schemas/EvalRunRequest" } } } }, "responses": { "200": { "description": "eval report" } } } },
            "/context/snapshot": { "get": { "operationId": "contextSnapshot", "responses": { "200": { "description": "situation snapshot" } } } },
            "/perception/events": { "get": { "operationId": "perceptionSubscribe", "responses": { "200": { "description": "SSE perception event stream" } } } },
            "/perception/capture": { "post": { "operationId": "perceptionCapture", "responses": { "200": { "description": "capture meta with OCR summary" } } } },
            "/perception/layers": { "post": { "operationId": "perceptionLayers", "responses": { "200": { "description": "layer authorization updated" } } } },
            "/perception/tree": { "post": { "operationId": "perceptionTree", "responses": { "200": { "description": "deep UI tree dump" } } } },
            "/perception/ocr": { "post": { "operationId": "perceptionOcr", "responses": { "200": { "description": "OCR text with bounding boxes" } } } },
            "/perception/ocr/status": { "get": { "operationId": "ocrStatus", "responses": { "200": { "description": "OCR engine diagnostics" } } } },
            "/perception/ocr/region": { "post": { "operationId": "perceptionOcrRegion", "responses": { "200": { "description": "region OCR text with bounding boxes" } } } },
            "/learn/record": { "post": { "operationId": "learnRecord", "responses": { "200": { "description": "learn state" } } } },
            "/learn/start": { "post": { "operationId": "learnStart", "responses": { "200": { "description": "learn state" } } } },
            "/learn/pause": { "post": { "operationId": "learnPause", "responses": { "200": { "description": "learn state" } } } },
            "/learn/resume": { "post": { "operationId": "learnResume", "responses": { "200": { "description": "learn state" } } } },
            "/learn/stop": { "post": { "operationId": "learnStop", "responses": { "200": { "description": "stopped with sample count" } } } },
            "/learn/clear": { "post": { "operationId": "learnClear", "responses": { "200": { "description": "ok" } } } },
            "/learn/execute": { "post": { "operationId": "learnExecute", "responses": { "200": { "description": "execution report" } } } },
            "/learn/packages": { "get": { "operationId": "learnPackages", "responses": { "200": { "description": "flow skill packages" } } } },
            "/learn/packages/{name}": { "get": { "operationId": "learnPackageDetail", "parameters": [path_param("name")], "responses": { "200": { "description": "package detail" } } }, "delete": { "operationId": "learnPackageDelete", "parameters": [path_param("name")], "responses": { "200": { "description": "deleted" } } } },
            "/learn/sink": { "post": { "operationId": "learnSink", "responses": { "200": { "description": "sunk package" } } } },
            "/learn/execute-package": { "post": { "operationId": "learnExecutePackage", "responses": { "200": { "description": "execution report" } } } },
            "/learn/export/{name}": { "get": { "operationId": "learnExport", "parameters": [path_param("name")], "responses": { "200": { "description": "owskill zip" } } } },
            "/learn/import": { "post": { "operationId": "learnImport", "responses": { "200": { "description": "imported package" } } } },
            "/skill/verify": { "post": { "operationId": "skillVerify", "responses": { "200": { "description": "validation result" } } } },
            "/proactive/observe": { "post": { "operationId": "proactiveObserve", "responses": { "200": { "description": "optional suggestion" } } } },
            "/proactive/decide": { "post": { "operationId": "proactiveDecide", "responses": { "200": { "description": "ok" } } } },
            "/proactive/suggestions": { "get": { "operationId": "proactiveSuggestions", "responses": { "200": { "description": "suggestion list" } } } },
            "/stt/transcribe": { "post": { "operationId": "sttTranscribe", "responses": { "200": { "description": "transcription text" } } } },
            "/automations": { "get": { "operationId": "automationsList", "responses": { "200": { "description": "automation tasks" } } }, "post": { "operationId": "automationsCreate", "responses": { "200": { "description": "created task" } } } },
            "/automations/{id}/toggle": { "post": { "operationId": "automationsToggle", "parameters": [path_param("id")], "responses": { "200": { "description": "enabled state" } } } },
            "/automations/{id}": { "delete": { "operationId": "automationsDelete", "parameters": [path_param("id")], "responses": { "200": { "description": "ok" } } } },
            "/automations/reminders": { "get": { "operationId": "automationsReminders", "responses": { "200": { "description": "pending reminders" } } } },
            "/automations/reminders/clear": { "post": { "operationId": "automationsClearReminders", "responses": { "200": { "description": "ok" } } } },
            "/settings": { "get": { "operationId": "settingsGet", "responses": { "200": { "description": "workspace settings" } } }, "post": { "operationId": "settingsUpdate", "responses": { "200": { "description": "workspace settings" } } } },
            "/settings/egress": { "post": { "operationId": "settingsEgress", "responses": { "200": { "description": "cloud enabled state" } } } },
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
                        "updated_at": { "type": "string" },
                        "title": { "type": "string" },
                        "archived": { "type": "boolean" },
                        "pinned": { "type": "boolean" },
                        "parent_id": { "type": "string" },
                        "fork_point": { "type": "integer" },
                        "model": { "type": "string" },
                        "created_at": { "type": "string" }
                    }
                },
                "TurnRequest": {
                    "type": "object",
                    "properties": {
                        "prompt": { "type": "string" },
                        "attachments": { "type": "array", "items": { "type": "string" } }
                    },
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
        updated_at: session.updated_at.clone(),
        title: Some(session.display_title()),
        archived: session.archived,
        pinned: session.pinned,
        parent_id: session.parent_id.clone(),
        fork_point: session.fork_point,
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

/// 把 Agent 内存审计日志中尚未落库的条目追加到存储，返回已 flush 数。
fn flush_audit(state: &AppState) {
    let mut flushed = match state.audit_flushed.lock() {
        Ok(flushed) => flushed,
        Err(_) => return,
    };
    let log = state.agent.audit_log();
    let audit = match log.lock() {
        Ok(audit) => audit,
        Err(_) => return,
    };
    if audit.entries.len() > *flushed {
        let entries = audit.entries[*flushed..].to_vec();
        *flushed = audit.entries.len();
        drop(audit);
        let _ = state.store.append_audit(&entries);
    }
}

async fn audit_list(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<owo_agent_core::AuditEntry>>, (StatusCode, String)> {
    flush_audit(&state);
    let limit = params
        .get("limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100)
        .min(500);
    Ok(Json(state.store.recent_audit(limit)))
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
    sessions.sort_by(|left, right| {
        right
            .pinned
            .cmp(&left.pinned)
            .then_with(|| right.updated_at.cmp(&left.updated_at))
    });
    Ok(Json(sessions))
}

async fn list_skills(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<Value>>, (StatusCode, String)> {
    let registry = state.agent.skills();
    let skills = registry.list();
    Ok(Json(
        skills
            .iter()
            .map(|skill| {
                json!({
                    "name": skill.name,
                    "description": skill.description,
                    "path": skill.path.to_string_lossy(),
                    "enabled": registry.is_enabled(&skill.name),
                })
            })
            .collect(),
    ))
}

async fn skill_detail(
    State(state): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let registry = state.agent.skills();
    let skill = registry
        .get(&name)
        .ok_or((StatusCode::NOT_FOUND, format!("技能不存在：{name}")))?;
    let content = std::fs::read_to_string(&skill.path)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(json!({
        "name": skill.name,
        "description": skill.description,
        "path": skill.path.to_string_lossy(),
        "enabled": registry.is_enabled(&name),
        "content": content,
    })))
}

#[derive(serde::Deserialize)]
struct SkillEditRequest {
    content: String,
}

async fn skill_edit(
    State(state): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
    Json(request): Json<SkillEditRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let skill = state
        .agent
        .skills()
        .get(&name)
        .ok_or((StatusCode::NOT_FOUND, format!("技能不存在：{name}")))?;
    std::fs::write(&skill.path, &request.content)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            "skills",
            "edit",
            Some(name.clone()),
            Some(true),
            "SKILL.md 已更新",
        );
    }
    Ok(Json(json!({
        "ok": true,
        "note": "SKILL.md 已更新（注册表内技能重启核心服务后生效）",
    })))
}

#[derive(serde::Deserialize)]
struct SkillEnabledRequest {
    enabled: bool,
}

async fn skill_enabled(
    State(state): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
    Json(request): Json<SkillEnabledRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let registry = state.agent.skills();
    if registry.get(&name).is_none() {
        return Err((StatusCode::NOT_FOUND, format!("技能不存在：{name}")));
    }
    let disabled = registry.disabled_set();
    {
        let mut set = disabled.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "禁用集合锁中毒".to_string(),
            )
        })?;
        if request.enabled {
            set.remove(&name);
        } else {
            set.insert(name.clone());
        }
    }
    let mut settings = owo_agent_core::Settings::load(&state.workspace);
    let mut list = {
        let set = disabled.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "禁用集合锁中毒".to_string(),
            )
        })?;
        let mut list: Vec<String> = set.iter().cloned().collect();
        list.sort();
        list
    };
    settings.skills.disabled = std::mem::take(&mut list);
    settings
        .save(&state.workspace)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            "skills",
            "enabled",
            Some(name.clone()),
            Some(request.enabled),
            format!(
                "技能{}：{name}",
                if request.enabled { "启用" } else { "禁用" }
            ),
        );
    }
    Ok(Json(json!({
        "ok": true,
        "enabled": request.enabled,
        "note": "已即时生效",
    })))
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
    let title = session.display_title();
    Ok(Json(SessionInfo {
        id: session.id,
        workspace: request.workspace,
        model: session.model,
        created_at: session.created_at,
        updated_at: session.updated_at,
        title: Some(title),
        archived: session.archived,
        pinned: session.pinned,
        parent_id: session.parent_id,
        fork_point: session.fork_point,
    }))
}

async fn get_session(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let session = load_session(&state, &id)?;
    Ok(Json(json!({
        "id": session.id,
        "title": session.display_title(),
        "model": session.model,
        "workspace": session.workspace.to_string_lossy(),
        "created_at": session.created_at,
        "updated_at": session.updated_at,
        "archived": session.archived,
        "pinned": session.pinned,
        "parent_id": session.parent_id,
        "fork_point": session.fork_point,
        "messages": session.messages,
    })))
}

async fn turn(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<TurnRequest>,
) -> Result<Sse<ReceiverStream<Result<Event, Infallible>>>, (StatusCode, String)> {
    let session = load_session(&state, &id)?;
    let mut effective_prompt = request.prompt.clone();
    if !request.attachments.is_empty() {
        let dir = attachment_dir(&state, &id);
        let mut lines = Vec::new();
        for attachment in &request.attachments {
            let safe = Path::new(attachment)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(attachment);
            let path = dir.join(safe);
            if !path.is_file() {
                return Err((StatusCode::BAD_REQUEST, format!("附件不存在：{safe}")));
            }
            let size = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
            lines.push(format!(
                "- {}（{} 字节，路径 {}）",
                safe,
                size,
                path.display()
            ));
        }
        effective_prompt.push_str("\n\n附件：\n");
        effective_prompt.push_str(&lines.join("\n"));
    }

    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(128);
    let approver = ChannelApprover {
        pending: Arc::clone(&state.pending_approvals),
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
    let state_for_audit = Arc::clone(&state);
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
                &effective_prompt,
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
        flush_audit(&state_for_audit);
    });

    Ok(Sse::new(ReceiverStream::new(rx)))
}

fn attachment_dir(state: &AppState, session_id: &str) -> std::path::PathBuf {
    state.workspace.join(".owo-attachments").join(session_id)
}

fn sanitize_attachment_name(name: &str) -> Option<String> {
    let file_name = Path::new(name).file_name()?.to_str()?;
    let cleaned: String = file_name
        .chars()
        .filter(|character| {
            !matches!(
                character,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
            )
        })
        .collect();
    let trimmed = cleaned.trim().to_string();
    if trimmed.is_empty() || trimmed.len() > 200 {
        None
    } else {
        Some(trimmed)
    }
}

#[derive(serde::Deserialize)]
struct AttachmentUploadRequest {
    name: String,
    #[serde(default)]
    mime: Option<String>,
    data_b64: String,
}

async fn attachment_upload(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<AttachmentUploadRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    load_session(&state, &id)?;
    let safe_name = sanitize_attachment_name(&request.name)
        .ok_or((StatusCode::BAD_REQUEST, "附件名非法".to_string()))?;
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&request.data_b64)
        .map_err(|error| {
            (
                StatusCode::BAD_REQUEST,
                format!("附件 base64 解码失败：{error}"),
            )
        })?;
    if bytes.len() > 50 * 1024 * 1024 {
        return Err((StatusCode::BAD_REQUEST, "附件超过 50MB 上限".to_string()));
    }
    let dir = attachment_dir(&state, &id);
    std::fs::create_dir_all(&dir)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let path = dir.join(&safe_name);
    std::fs::write(&path, &bytes)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            &id,
            "attachment",
            Some(safe_name.clone()),
            Some(true),
            format!("上传附件 {}（{} 字节）", safe_name, bytes.len()),
        );
    }
    Ok(Json(json!({
        "id": safe_name,
        "name": request.name,
        "mime": request.mime,
        "size": bytes.len(),
        "path": path.to_string_lossy(),
    })))
}

async fn attachments_list(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Vec<Value>>, (StatusCode, String)> {
    load_session(&state, &id)?;
    let dir = attachment_dir(&state, &id);
    let mut attachments = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let size = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
            attachments.push(json!({ "id": name, "name": name, "size": size }));
        }
    }
    attachments.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    Ok(Json(attachments))
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

#[derive(serde::Deserialize)]
struct RenameRequest {
    title: String,
}

#[derive(serde::Deserialize)]
struct ArchiveRequest {
    archived: bool,
}

#[derive(serde::Deserialize)]
struct PinRequest {
    pinned: bool,
}

async fn session_rename(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<RenameRequest>,
) -> Result<Json<SessionInfo>, (StatusCode, String)> {
    let mut session = load_session(&state, &id)?;
    session.rename(request.title);
    state
        .store
        .save(&session)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    state
        .sessions
        .lock()
        .map_err(poison)?
        .insert(session.id.clone(), session.clone());
    Ok(Json(to_session_info(&session)))
}

async fn session_archive(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<ArchiveRequest>,
) -> Result<Json<SessionInfo>, (StatusCode, String)> {
    let mut session = load_session(&state, &id)?;
    session.set_archived(request.archived);
    state
        .store
        .save(&session)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    state
        .sessions
        .lock()
        .map_err(poison)?
        .insert(session.id.clone(), session.clone());
    Ok(Json(to_session_info(&session)))
}

async fn session_pin(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<PinRequest>,
) -> Result<Json<SessionInfo>, (StatusCode, String)> {
    let mut session = load_session(&state, &id)?;
    session.set_pinned(request.pinned);
    state
        .store
        .save(&session)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    state
        .sessions
        .lock()
        .map_err(poison)?
        .insert(session.id.clone(), session.clone());
    Ok(Json(to_session_info(&session)))
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
struct TreeDumpRequest {
    #[serde(default = "default_tree_depth")]
    max_depth: u32,
    #[serde(default = "default_tree_nodes")]
    max_nodes: usize,
    /// 可选：按窗口句柄抓树（不要求前台），用于窗口模板/后台情景理解。
    #[serde(default)]
    hwnd: Option<i64>,
}

fn default_tree_depth() -> u32 {
    12
}

fn default_tree_nodes() -> usize {
    1000
}

/// 深度 UI 树转储（computer-use 调试：找深层语义锚点，如 QQ 工具栏按钮）。
async fn perception_tree(
    Json(request): Json<TreeDumpRequest>,
) -> Result<Json<Vec<owo_agent_core::UiNode>>, (StatusCode, String)> {
    let tree = match request.hwnd {
        Some(hwnd) => {
            owo_agent_core::ui_tree_for_hwnd(hwnd as isize, request.max_depth, request.max_nodes)
        }
        None => owo_agent_core::foreground_ui_tree(request.max_depth, request.max_nodes),
    };
    tree.map(Json)
        .ok_or((StatusCode::BAD_REQUEST, "无法获取 UI 树".to_string()))
}

#[derive(serde::Deserialize)]
struct TemplateBuildRequest {
    hwnd: i64,
    app_id: String,
}

async fn perception_template_build(
    State(state): State<Arc<AppState>>,
    Json(request): Json<TemplateBuildRequest>,
) -> Result<Json<owo_agent_core::WindowTemplate>, (StatusCode, String)> {
    let tree = owo_agent_core::ui_tree_for_hwnd(request.hwnd as isize, 14, 10000)
        .ok_or((StatusCode::BAD_REQUEST, "无法获取窗口 UI 树".to_string()))?;
    let template = owo_agent_core::build_template(&request.app_id, &tree);
    owo_agent_core::save_template(&state.data_root, &template)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            "template",
            "build",
            Some(request.app_id.clone()),
            Some(true),
            format!(
                "构建窗口模板：{}（{} 个 ROI）",
                request.app_id,
                template.rois.len()
            ),
        );
    }
    Ok(Json(template))
}

async fn perception_template_get(
    State(state): State<Arc<AppState>>,
    AxumPath(app_id): AxumPath<String>,
) -> Result<Json<owo_agent_core::WindowTemplate>, (StatusCode, String)> {
    owo_agent_core::load_template(&state.data_root, &app_id)
        .map(Json)
        .ok_or((StatusCode::NOT_FOUND, format!("窗口模板不存在：{app_id}")))
}

#[derive(serde::Deserialize)]
struct TemplateDetectRequest {
    hwnd: i64,
    app_id: String,
}

async fn perception_template_detect(
    State(state): State<Arc<AppState>>,
    Json(request): Json<TemplateDetectRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let template = owo_agent_core::load_template(&state.data_root, &request.app_id).ok_or((
        StatusCode::NOT_FOUND,
        format!("窗口模板不存在：{}", request.app_id),
    ))?;
    let tree = owo_agent_core::ui_tree_for_hwnd(request.hwnd as isize, 14, 10000)
        .ok_or((StatusCode::BAD_REQUEST, "无法获取窗口 UI 树".to_string()))?;
    Ok(Json(owo_agent_core::detect_template(&template, &tree)))
}

/// OCR 版模板构建：PrintWindow 抓窗口 → PP-OCRv6 → 按语义文本提取 ROI（后台可用）。
async fn perception_template_build_ocr(
    State(state): State<Arc<AppState>>,
    Json(request): Json<TemplateBuildRequest>,
) -> Result<Json<owo_agent_core::WindowTemplate>, (StatusCode, String)> {
    let (bmp, _rect) = owo_agent_core::platform::capture_window_bmp_deep(request.hwnd as isize)
        .ok_or((StatusCode::BAD_REQUEST, "窗口截图失败".to_string()))?;
    let summary = owo_agent_core::ocr_preferred(&bmp)
        .await
        .map_err(|error| (StatusCode::BAD_GATEWAY, error))?;
    let template = owo_agent_core::build_template_from_ocr(&request.app_id, &summary);
    owo_agent_core::save_template(&state.data_root, &template)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    Ok(Json(template))
}

/// OCR 版模板检测：当前窗口 OCR 行中心 vs 模板 ROI 命中率。
async fn perception_template_detect_ocr(
    State(state): State<Arc<AppState>>,
    Json(request): Json<TemplateDetectRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let template = owo_agent_core::load_template(&state.data_root, &request.app_id).ok_or((
        StatusCode::NOT_FOUND,
        format!("窗口模板不存在：{}", request.app_id),
    ))?;
    let (bmp, _rect) = owo_agent_core::platform::capture_window_bmp_deep(request.hwnd as isize)
        .ok_or((StatusCode::BAD_REQUEST, "窗口截图失败".to_string()))?;
    let summary = owo_agent_core::ocr_preferred(&bmp)
        .await
        .map_err(|error| (StatusCode::BAD_GATEWAY, error))?;
    Ok(Json(owo_agent_core::detect_template_ocr(
        &template, &summary,
    )))
}

/// 全屏 OCR（含文本框坐标），供 OCR+坐标点击（自绘面板，如 QQ 红包/表情）。
async fn perception_ocr(
    State(state): State<Arc<AppState>>,
) -> Result<Json<owo_agent_core::OcrSummary>, (StatusCode, String)> {
    if !state
        .perception
        .lock()
        .map_err(poison)?
        .is_enabled(owo_agent_core::PerceptionLayer::L2Visual)
    {
        return Err((StatusCode::BAD_REQUEST, "L2 视觉层未授权".to_string()));
    }
    let bytes = owo_agent_core::capture_screen()
        .ok_or((StatusCode::BAD_REQUEST, "屏幕截图失败".to_string()))?;
    owo_agent_core::ocr_preferred(&bytes)
        .await
        .map(Json)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))
}

async fn ocr_status() -> Json<owo_agent_core::OcrEngineStatus> {
    Json(owo_agent_core::ocr_engine_status())
}

#[derive(serde::Deserialize)]
struct OcrBytesRequest {
    bmp_b64: String,
}

/// 对 base64 编码的 BMP 做 OCR（模拟窗口帧/附件截图调试用，不依赖屏幕）。
async fn perception_ocr_bytes(
    Json(request): Json<OcrBytesRequest>,
) -> Result<Json<owo_agent_core::OcrSummary>, (StatusCode, String)> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&request.bmp_b64)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("base64 解码失败：{e}")))?;
    owo_agent_core::ocr_preferred(&bytes)
        .await
        .map(Json)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))
}

#[derive(serde::Deserialize)]
struct OcrRegionRequest {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    #[serde(default = "default_ocr_scale")]
    scale: u32,
}

fn default_ocr_scale() -> u32 {
    2
}

/// 区域 OCR：裁剪 + 放大后识别（小字验证窗口/自绘面板）。
async fn perception_ocr_region(
    State(state): State<Arc<AppState>>,
    Json(request): Json<OcrRegionRequest>,
) -> Result<Json<owo_agent_core::OcrSummary>, (StatusCode, String)> {
    if !state
        .perception
        .lock()
        .map_err(poison)?
        .is_enabled(owo_agent_core::PerceptionLayer::L2Visual)
    {
        return Err((StatusCode::BAD_REQUEST, "L2 视觉层未授权".to_string()));
    }
    let bytes = owo_agent_core::capture_screen()
        .ok_or((StatusCode::BAD_REQUEST, "屏幕截图失败".to_string()))?;
    let cropped = owo_agent_core::crop_scale_bmp(
        &bytes,
        request.x,
        request.y,
        request.width,
        request.height,
        request.scale,
    )
    .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    owo_agent_core::ocr_preferred(&cropped)
        .await
        .map(Json)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))
}

#[derive(serde::Deserialize)]
struct WindowOcrRequest {
    hwnd: i64,
}

/// 窗口级 OCR：PrintWindow 后台只读抓取指定窗口 → PP-OCRv6/Media 识别，返回窗口矩形与文本行。
async fn perception_window(
    Json(request): Json<WindowOcrRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (bmp, rect) = owo_agent_core::platform::capture_window_bmp_deep(request.hwnd as isize)
        .ok_or((StatusCode::BAD_REQUEST, "窗口截图失败".to_string()))?;
    let summary = owo_agent_core::ocr_preferred(&bmp)
        .await
        .map_err(|error| (StatusCode::BAD_GATEWAY, error))?;
    let lines: Vec<Value> = owo_agent_core::group_ocr_lines(&summary.boxes)
        .into_iter()
        .map(|line| {
            json!({
                "text": line.text,
                "x": line.x,
                "y": line.y,
                "width": line.width,
                "height": line.height,
            })
        })
        .collect();
    Ok(Json(json!({
        "window_rect": [rect.0, rect.1, rect.2, rect.3],
        "provider": summary.provider,
        "chars": summary.chars,
        "text": summary.text,
        "lines": lines,
        "boxes": summary.boxes,
    })))
}

#[derive(serde::Deserialize)]
struct DesktopClickRequest {
    x: i32,
    y: i32,
}

#[derive(serde::Deserialize)]
struct DesktopTextRequest {
    text: String,
}

#[derive(serde::Deserialize)]
struct DesktopKeyRequest {
    key: String,
}

#[derive(serde::Deserialize)]
struct DesktopComboRequest {
    combo: String,
}

#[derive(serde::Deserialize)]
struct DesktopTargetRequest {
    target: String,
}

#[derive(serde::Deserialize)]
struct DesktopScrollRequest {
    x: i32,
    y: i32,
    delta: i32,
}

#[derive(serde::Deserialize)]
struct DesktopActivateRequest {
    #[serde(default)]
    process: String,
    #[serde(default)]
    title: String,
}

#[derive(serde::Deserialize)]
struct DesktopWaitRequest {
    ms: u64,
}

async fn desktop_foreground() -> Json<Value> {
    if std::env::var("OWO_SIM_QQ_URL")
        .map(|value| !value.is_empty())
        .unwrap_or(false)
    {
        return Json(json!({
            "process": "owo-sim-qq",
            "title": "OwO 模拟QQ - 张子豪",
            "rect": [0, 0, 1020, 700],
            "surface": "sim",
        }));
    }
    let (process, title) = owo_agent_core::poll_foreground_app().unwrap_or_default();
    let rect = owo_agent_core::platform::foreground_window_rect();
    Json(json!({ "process": process, "title": title, "rect": rect }))
}

async fn desktop_windows() -> Json<Value> {
    if std::env::var("OWO_SIM_QQ_URL")
        .map(|value| !value.is_empty())
        .unwrap_or(false)
    {
        return Json(json!({
            "windows": [{
                "hwnd": 1,
                "pid": 1,
                "process": "owo-sim-qq",
                "title": "OwO 模拟QQ - 张子豪",
                "rect": [0, 0, 1020, 700],
                "visible": true,
            }],
            "surface": "sim",
        }));
    }
    Json(json!({ "windows": owo_agent_core::platform::window_list() }))
}

async fn desktop_activate(
    Json(request): Json<DesktopActivateRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    ensure_real_desktop("desktop_activate")?;
    owo_agent_core::platform::activate_window(&request.process, &request.title)
        .map(|_| Json(json!({ "ok": true })))
        .map_err(|error| (StatusCode::BAD_REQUEST, error))
}

async fn desktop_click(
    Json(request): Json<DesktopClickRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    ensure_real_desktop("desktop_click")?;
    owo_agent_core::computer_use::desktop_click(request.x, request.y)
        .map(|_| Json(json!({ "ok": true, "x": request.x, "y": request.y })))
        .map_err(|error| (StatusCode::BAD_REQUEST, error))
}

async fn desktop_type(
    Json(request): Json<DesktopTextRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    ensure_real_desktop("desktop_type")?;
    owo_agent_core::computer_use::desktop_type(&request.text)
        .map(|_| Json(json!({ "ok": true, "typed_chars": request.text.chars().count() })))
        .map_err(|error| (StatusCode::BAD_REQUEST, error))
}

async fn desktop_key(
    Json(request): Json<DesktopKeyRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    ensure_real_desktop("desktop_key")?;
    owo_agent_core::computer_use::desktop_key(&request.key)
        .map(|_| Json(json!({ "ok": true, "key": request.key })))
        .map_err(|error| (StatusCode::BAD_REQUEST, error))
}

async fn desktop_shortcut(
    Json(request): Json<DesktopComboRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    ensure_real_desktop("desktop_shortcut")?;
    owo_agent_core::computer_use::desktop_shortcut(&request.combo)
        .map(|_| Json(json!({ "ok": true, "combo": request.combo })))
        .map_err(|error| (StatusCode::BAD_REQUEST, error))
}

async fn desktop_launch(
    Json(request): Json<DesktopTargetRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    ensure_real_desktop("desktop_launch")?;
    owo_agent_core::computer_use::desktop_launch(&request.target)
        .map(|_| Json(json!({ "ok": true, "target": request.target })))
        .map_err(|error| (StatusCode::BAD_REQUEST, error))
}

async fn desktop_scroll(
    Json(request): Json<DesktopScrollRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    ensure_real_desktop("desktop_scroll")?;
    owo_agent_core::computer_use::desktop_scroll(request.x, request.y, request.delta)
        .map(|_| {
            Json(json!({ "ok": true, "x": request.x, "y": request.y, "delta": request.delta }))
        })
        .map_err(|error| (StatusCode::BAD_REQUEST, error))
}

async fn desktop_wait(Json(request): Json<DesktopWaitRequest>) -> Json<Value> {
    let ms = request.ms.min(120_000);
    tokio::time::sleep(Duration::from_millis(ms)).await;
    Json(json!({ "waited_ms": ms }))
}

async fn vision_status() -> Json<Value> {
    let config = owo_agent_core::VisionConfig::from_env();
    let models = if config.provider == "ollama" {
        owo_agent_core::ollama_models(&config).await
    } else {
        Vec::new()
    };
    Json(json!({
        "provider": config.provider,
        "model": config.model,
        "ollama_host": config.ollama_host,
        "ollama_models": models,
    }))
}

#[derive(serde::Deserialize)]
struct VisionDescribeRequest {
    #[serde(default)]
    prompt: Option<String>,
}

async fn vision_describe(
    Json(request): Json<VisionDescribeRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (png, surface) = owo_agent_core::capture_vision_png()
        .await
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    let prompt = request.prompt.unwrap_or_else(|| {
        "请用中文描述这个界面的当前状态：这是什么应用？有哪些关键控件（按钮/输入框/消息）？\
         它们大致在什么位置？最新消息内容是什么？"
            .to_string()
    });
    let description = owo_agent_core::describe_image(&png, &prompt)
        .await
        .map_err(|error| (StatusCode::BAD_GATEWAY, error))?;
    let config = owo_agent_core::VisionConfig::from_env();
    Ok(Json(json!({
        "surface": surface,
        "provider": config.provider,
        "model": config.model,
        "description": description,
    })))
}

#[derive(serde::Deserialize)]
struct VisionVerifyRequest {
    question: String,
}

/// 视觉完成验证：对当前截图回答 yes/no 问题，返回 answer + confidence。
async fn vision_verify(
    Json(request): Json<VisionVerifyRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (png, surface) = owo_agent_core::capture_vision_png()
        .await
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    let prompt = format!(
        "请只看这张截图回答问题。先回答 YES 或 NO，再给出 0-1 置信度。问题：{}",
        request.question
    );
    let raw = owo_agent_core::describe_image(&png, &prompt)
        .await
        .map_err(|error| (StatusCode::BAD_GATEWAY, error))?;
    let (answer, confidence) = owo_agent_core::parse_verification(&raw);
    let config = owo_agent_core::VisionConfig::from_env();
    Ok(Json(json!({
        "surface": surface,
        "provider": config.provider,
        "model": config.model,
        "question": request.question,
        "answer": answer,
        "confidence": confidence,
        "raw": raw,
    })))
}

#[derive(serde::Deserialize)]
struct VisionGroundRequest {
    description: String,
}

/// 视觉 grounding：视觉模型给框 → 与 OCR 文本交叉验证后才允许点击。
async fn vision_ground(
    Json(request): Json<VisionGroundRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    owo_agent_core::ground_element(&request.description)
        .await
        .map(Json)
        .map_err(|error| (StatusCode::BAD_GATEWAY, error))
}

async fn memory_observations(
    State(state): State<Arc<AppState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let limit = params
        .get("limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100);
    let memory = state.memory.lock().map_err(poison)?;
    let observations = memory.list(limit);
    Ok(Json(json!({
        "count": observations.len(),
        "total": memory.count(),
        "observations": observations,
    })))
}

async fn memory_clear(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut memory = state.memory.lock().map_err(poison)?;
    memory
        .clear()
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(serde::Deserialize)]
struct MineSkillRequest {
    name: String,
    target_apps: Vec<String>,
    sensitivity: String,
    description: String,
}

/// 从情景记忆自动挖掘流程技能：观察到的动作序列 → 泛化 → 沉淀技能包。
async fn memory_mine_skill(
    State(state): State<Arc<AppState>>,
    Json(request): Json<MineSkillRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let sensitivity = parse_sensitivity(&request.sensitivity)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    let actions = {
        let memory = state.memory.lock().map_err(poison)?;
        let observations = memory.list(0);
        owo_agent_core::map_sim_events_to_actions(&observations)
    };
    if actions.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "情景记忆中没有可挖掘的动作（请先运行模拟/真实操作并等待观察器入库）".to_string(),
        ));
    }
    let mut pipeline = state.pipeline.lock().map_err(poison)?;
    pipeline.recorder.start();
    for action in actions {
        pipeline
            .recorder
            .record(action)
            .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    }
    let package = pipeline
        .sink_skill(
            &request.name,
            request.target_apps,
            sensitivity,
            &request.description,
        )
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            "memory",
            "mine-skill",
            Some(package.manifest.name.clone()),
            Some(true),
            format!("从情景记忆挖掘技能包：{}", package.manifest.name),
        );
    }
    Ok(Json(json!({
        "ok": true,
        "name": package.manifest.name,
        "variables": package.manifest.variables,
    })))
}

fn ensure_real_desktop(tool: &str) -> Result<(), (StatusCode, String)> {
    if std::env::var("OWO_SIM_QQ_URL")
        .map(|value| !value.is_empty())
        .unwrap_or(false)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("{tool} 在模拟环境下被禁用：请直连模拟服务或通过 Agent 工具执行",),
        ));
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct LearnRecordRequest {
    action: RecordedAction,
}

async fn learn_start(
    State(state): State<Arc<AppState>>,
) -> Result<Json<LearnState>, (StatusCode, String)> {
    let mut pipeline = state.pipeline.lock().map_err(poison)?;
    pipeline.recorder.start();
    Ok(Json(pipeline.recorder.state()))
}

async fn learn_record(
    State(state): State<Arc<AppState>>,
    Json(request): Json<LearnRecordRequest>,
) -> Result<Json<LearnState>, (StatusCode, String)> {
    let mut pipeline = state.pipeline.lock().map_err(poison)?;
    pipeline
        .recorder
        .record(request.action)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    Ok(Json(pipeline.recorder.state()))
}

async fn learn_pause(
    State(state): State<Arc<AppState>>,
) -> Result<Json<LearnState>, (StatusCode, String)> {
    let mut pipeline = state.pipeline.lock().map_err(poison)?;
    pipeline.recorder.pause();
    Ok(Json(pipeline.recorder.state()))
}

async fn learn_resume(
    State(state): State<Arc<AppState>>,
) -> Result<Json<LearnState>, (StatusCode, String)> {
    let mut pipeline = state.pipeline.lock().map_err(poison)?;
    pipeline.recorder.resume();
    Ok(Json(pipeline.recorder.state()))
}

async fn learn_stop(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut pipeline = state.pipeline.lock().map_err(poison)?;
    let samples = pipeline.stop_recording().len();
    Ok(Json(json!({
        "state": pipeline.recorder.state(),
        "samples": samples,
    })))
}

async fn learn_clear(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut pipeline = state.pipeline.lock().map_err(poison)?;
    pipeline.recorder.clear();
    Ok(Json(json!({ "ok": true })))
}

async fn learn_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pipeline = state.pipeline.lock().map_err(poison)?;
    Ok(Json(json!({
        "state": pipeline.recorder.state(),
        "samples": pipeline.recorder.samples(),
        "sensitive_break": pipeline.recorder.sensitive_break(),
    })))
}

#[derive(serde::Deserialize)]
struct ExecuteRequest {
    graph: owo_agent_core::ActionGraph,
    #[serde(default)]
    variables: std::collections::HashMap<String, String>,
    #[serde(default)]
    max_steps: Option<usize>,
    /// 首次执行必须显式确认（服务端强制审批）。
    #[serde(default)]
    confirm: bool,
}

/// 执行流程技能包动作图（Windows：UI Automation + SendInput，敏感面熔断）。
async fn learn_execute(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ExecuteRequest>,
) -> Result<Json<owo_agent_core::ExecReport>, (StatusCode, String)> {
    if !request.confirm {
        return Err((
            StatusCode::BAD_REQUEST,
            "首次执行必须确认（confirm: true）".to_string(),
        ));
    }
    let source = ui_action_source()?;
    let report = owo_agent_core::execute_graph(
        source.as_ref(),
        &request.graph,
        &request.variables,
        request.max_steps.unwrap_or(20),
    );
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        for step in &report.steps {
            audit.record(
                "learn-execute",
                "exec",
                Some(step.node_id.clone()),
                Some(step.status == "ok"),
                step.detail.clone(),
            );
        }
    }
    Ok(Json(report))
}

fn parse_sensitivity(value: &str) -> Result<Sensitivity, String> {
    match value {
        "low" => Ok(Sensitivity::Low),
        "medium" => Ok(Sensitivity::Medium),
        "high" => Ok(Sensitivity::High),
        "none" => Ok(Sensitivity::None),
        other => Err(format!("未知敏感度：{other}（low/medium/high/none）")),
    }
}

/// 流程技能包列表（用户学习产物）。
async fn learn_packages(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<Value>>, (StatusCode, String)> {
    let pipeline = state.pipeline.lock().map_err(poison)?;
    let mut packages = Vec::new();
    for name in pipeline
        .store
        .list()
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?
    {
        if let Ok(package) = pipeline.store.load(&name) {
            packages.push(json!({
                "name": package.manifest.name,
                "target_apps": package.manifest.target_apps,
                "variables": package.manifest.variables,
                "sensitivity": package.manifest.sensitivity,
                "version": package.manifest.version,
            }));
        }
    }
    Ok(Json(packages))
}

async fn learn_package_detail(
    State(state): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pipeline = state.pipeline.lock().map_err(poison)?;
    let package = pipeline
        .store
        .load(&name)
        .map_err(|error| (StatusCode::NOT_FOUND, error))?;
    Ok(Json(json!({
        "name": package.manifest.name,
        "target_apps": package.manifest.target_apps,
        "variables": package.manifest.variables,
        "sensitivity": package.manifest.sensitivity,
        "version": package.manifest.version,
        "skill_md": package.skill_md,
        "graph": package.graph,
    })))
}

async fn learn_package_delete(
    State(state): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pipeline = state.pipeline.lock().map_err(poison)?;
    pipeline
        .store
        .delete(&name)
        .map_err(|error| (StatusCode::NOT_FOUND, error))?;
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            "learn",
            "delete-package",
            Some(name.clone()),
            Some(true),
            format!("删除流程技能包：{name}"),
        );
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(serde::Deserialize)]
struct SinkRequest {
    name: String,
    target_apps: Vec<String>,
    sensitivity: String,
    description: String,
}

/// 结束录制并沉淀为流程技能包。
async fn learn_sink(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SinkRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let sensitivity = parse_sensitivity(&request.sensitivity)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    let mut pipeline = state.pipeline.lock().map_err(poison)?;
    let package = pipeline
        .sink_skill(
            &request.name,
            request.target_apps,
            sensitivity,
            &request.description,
        )
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    Ok(Json(json!({
        "ok": true,
        "name": package.manifest.name,
        "variables": package.manifest.variables,
    })))
}

#[derive(serde::Deserialize)]
struct ExecutePackageRequest {
    name: String,
    #[serde(default)]
    variables: HashMap<String, String>,
    #[serde(default)]
    max_steps: Option<usize>,
    /// 首次执行必须显式确认（服务端强制审批）。
    #[serde(default)]
    confirm: bool,
    /// 高敏感（High）技能包需二次确认。
    #[serde(default)]
    high_risk_ack: bool,
}

/// 从流程技能包加载动作图并执行（首次执行需在 UI 确认，步审计入库）。
async fn learn_execute_package(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ExecutePackageRequest>,
) -> Result<Json<owo_agent_core::ExecReport>, (StatusCode, String)> {
    if !request.confirm {
        return Err((
            StatusCode::BAD_REQUEST,
            "首次执行必须确认（confirm: true）".to_string(),
        ));
    }
    let package = {
        let pipeline = state.pipeline.lock().map_err(poison)?;
        pipeline
            .store
            .load(&request.name)
            .map_err(|error| (StatusCode::NOT_FOUND, error))?
    };
    if package.manifest.sensitivity == Sensitivity::High && !request.high_risk_ack {
        return Err((
            StatusCode::BAD_REQUEST,
            "高敏感技能包需二次确认（high_risk_ack: true）".to_string(),
        ));
    }
    let source = ui_action_source()?;
    let report = owo_agent_core::execute_graph(
        source.as_ref(),
        &package.graph,
        &request.variables,
        request.max_steps.unwrap_or(20),
    );
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        if package.manifest.sensitivity == Sensitivity::High {
            audit.record(
                "learn-execute-package",
                "high_risk_ack",
                Some(request.name.clone()),
                Some(true),
                "高敏感技能包二次确认",
            );
        }
        audit.record(
            "learn-execute-package",
            "approval",
            Some(request.name.clone()),
            Some(true),
            "首次执行已确认",
        );
        for step in &report.steps {
            audit.record(
                "learn-execute-package",
                "exec",
                Some(step.node_id.clone()),
                Some(step.status == "ok"),
                step.detail.clone(),
            );
        }
    }
    Ok(Json(report))
}

/// 根据运行环境选择执行器源：模拟面用 SimUiActionSource（虚拟窗口），
/// 真实桌面用 WindowsUiaSource。
fn ui_action_source() -> Result<Box<dyn owo_agent_core::UiActionSource>, (StatusCode, String)> {
    if std::env::var("OWO_SIM_QQ_URL")
        .map(|value| !value.is_empty())
        .unwrap_or(false)
    {
        owo_agent_core::computer_use::SimUiActionSource::new()
            .map(|source| Box::new(source) as Box<dyn owo_agent_core::UiActionSource>)
            .map_err(|error| (StatusCode::BAD_REQUEST, error))
    } else {
        owo_agent_core::WindowsUiaSource::new()
            .map(|source| Box::new(source) as Box<dyn owo_agent_core::UiActionSource>)
            .map_err(|error| (StatusCode::BAD_REQUEST, error))
    }
}

/// 导出流程技能包为 `.owskill`（ZIP）。
async fn learn_export(
    State(state): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    let package = {
        let pipeline = state.pipeline.lock().map_err(poison)?;
        pipeline
            .store
            .load(&name)
            .map_err(|error| (StatusCode::NOT_FOUND, error))?
    };
    let bytes = owo_agent_core::export_flow_skill_package(&package)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    let disposition = format!("attachment; filename=\"{name}.owskill\"");
    Ok((
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/zip".to_string(),
            ),
            (axum::http::header::CONTENT_DISPOSITION, disposition),
        ],
        bytes,
    )
        .into_response())
}

/// 导入 `.owskill`（ZIP）并保存到用户技能包目录。
async fn learn_import(
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, String)> {
    let package = owo_agent_core::import_flow_skill_package(&body)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    let pipeline = state.pipeline.lock().map_err(poison)?;
    pipeline
        .store
        .save(&package)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    Ok(Json(json!({
        "ok": true,
        "name": package.manifest.name,
        "variables": package.manifest.variables,
        "target_apps": package.manifest.target_apps,
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

/// 主动建议列表（桌面端“学习/执行一次/忽略/静默”四选）。
async fn proactive_suggestions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<ProactiveSuggestion>>, (StatusCode, String)> {
    let proactive = state.proactive.lock().map_err(poison)?;
    Ok(Json(proactive.suggestions().to_vec()))
}

/// 本地离线转写：请求体为 WAV 字节（16k PCM），返回文本（SenseVoice-Small）。
async fn stt_transcribe(
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, String)> {
    let wav_path = std::env::temp_dir().join(format!("owo-stt-{}.wav", uuid::Uuid::new_v4()));
    std::fs::write(&wav_path, &body)
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    let (outcome, engine) = {
        let stt = state.stt.lock().map_err(poison)?;
        let outcome = stt
            .transcribe_wav(&wav_path)
            .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
        (outcome, stt.engine().to_string())
    };
    let _ = std::fs::remove_file(&wav_path);
    Ok(Json(json!({
        "ok": true,
        "text": outcome.text,
        "elapsed_ms": outcome.elapsed_ms,
        "engine": engine,
    })))
}

// ---------- 自动化 ----------

#[derive(serde::Deserialize)]
struct CreateAutomationRequest {
    name: String,
    schedule: Schedule,
    reminder: String,
}

async fn automations_list(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<AutomationTask>>, (StatusCode, String)> {
    let automations = state.automations.lock().map_err(poison)?;
    Ok(Json(automations.list()))
}

async fn automations_create(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateAutomationRequest>,
) -> Result<Json<AutomationTask>, (StatusCode, String)> {
    let task = AutomationTask::new(
        &request.name,
        request.schedule,
        AutomationAction::Reminder {
            text: request.reminder,
        },
    );
    let mut automations = state.automations.lock().map_err(poison)?;
    automations
        .upsert(task.clone())
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    Ok(Json(task))
}

async fn automations_toggle(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut automations = state.automations.lock().map_err(poison)?;
    let enabled = automations
        .toggle(&id)
        .map_err(|error| (StatusCode::NOT_FOUND, error))?;
    Ok(Json(json!({ "id": id, "enabled": enabled })))
}

async fn automations_delete(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut automations = state.automations.lock().map_err(poison)?;
    automations
        .remove(&id)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    Ok(Json(json!({ "ok": true })))
}

async fn automations_reminders(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<String>>, (StatusCode, String)> {
    let automations = state.automations.lock().map_err(poison)?;
    Ok(Json(automations.reminders().to_vec()))
}

async fn automations_clear_reminders(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut automations = state.automations.lock().map_err(poison)?;
    automations
        .clear_reminders()
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::sanitize_attachment_name;

    #[test]
    fn sanitizes_attachment_names() {
        assert_eq!(
            sanitize_attachment_name("report.pdf").as_deref(),
            Some("report.pdf")
        );
        assert_eq!(
            sanitize_attachment_name("a/b/c.txt").as_deref(),
            Some("c.txt")
        );
        assert_eq!(
            sanitize_attachment_name("..\\evil.txt").as_deref(),
            Some("evil.txt")
        );
        assert_eq!(
            sanitize_attachment_name("a:b*c?.txt").as_deref(),
            Some("bc.txt")
        );
        assert!(sanitize_attachment_name("").is_none());
        assert!(sanitize_attachment_name("   ").is_none());
        assert!(sanitize_attachment_name("x".repeat(201).as_str()).is_none());
    }
}

/// 自动化常驻循环：每秒检查到期任务，触发提醒并写审计。
pub async fn start_automation_loop(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    loop {
        interval.tick().await;
        let fired = {
            let mut automations = state
                .automations
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let now = chrono::Utc::now();
            let mut fired = Vec::new();
            for id in automations.due_tasks(now) {
                if let Ok(text) = automations.fire(&id, now) {
                    fired.push(text);
                }
            }
            fired
        };
        if !fired.is_empty() {
            if let Ok(mut audit) = state.agent.audit_log().lock() {
                audit.record("automation", "fire", None, Some(true), fired.join(" | "));
            }
        }
    }
}

/// 静默观察器：模拟面下每 2s 拉取模拟窗口日志，把动作摘要（内容掩码）写入情景记忆。
pub async fn start_memory_observer(state: Arc<AppState>) {
    let mut seen = 0usize;
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let Some(base) = std::env::var("OWO_SIM_QQ_URL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let url = format!("{}/log", base.trim_end_matches('/'));
        let Ok(response) = reqwest::get(&url).await else {
            continue;
        };
        let Ok(value) = response.json::<Value>().await else {
            continue;
        };
        let Some(entries) = value.get("entries").and_then(Value::as_array) else {
            continue;
        };
        if entries.len() < seen {
            // 模拟场景被 /reset 清空：从头重新计数。
            seen = 0;
        }
        if entries.len() <= seen {
            continue;
        }
        let mut memory = state
            .memory
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for entry in &entries[seen..] {
            if let Some(observation) = owo_agent_core::observation_from_sim_event(entry) {
                let _ = memory.append(observation);
            }
        }
        seen = entries.len();
    }
}

// ---------- 设置与诊断 ----------

async fn settings_get(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let settings = owo_agent_core::Settings::load(&state.workspace);
    serde_json::to_value(&settings)
        .map(Json)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
}

#[derive(serde::Deserialize)]
struct EgressRequest {
    cloud_enabled: bool,
}

async fn settings_egress(
    State(state): State<Arc<AppState>>,
    Json(request): Json<EgressRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut settings = owo_agent_core::Settings::load(&state.workspace);
    settings.egress.cloud_enabled = request.cloud_enabled;
    settings
        .save(&state.workspace)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    std::env::set_var(
        "OWO_CLOUD_ENABLED",
        if request.cloud_enabled {
            "true"
        } else {
            "false"
        },
    );
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            "settings",
            "egress",
            None,
            Some(request.cloud_enabled),
            format!("数据出境开关：cloud_enabled={}", request.cloud_enabled),
        );
    }
    Ok(Json(json!({
        "cloud_enabled": request.cloud_enabled,
        "note": "已写入 settings.json 并即时生效",
    })))
}

/// 通用设置保存：写入 settings.json 并应用运行时设置（数据出境、STT、主动建议、白名单）。
async fn settings_update(
    State(state): State<Arc<AppState>>,
    Json(settings): Json<owo_agent_core::Settings>,
) -> Result<Json<Value>, (StatusCode, String)> {
    settings
        .save(&state.workspace)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    if let Some(model) = &settings.model {
        if !model.trim().is_empty() {
            std::env::set_var("OPENAI_MODEL", model);
        }
    }
    std::env::set_var(
        "OWO_CLOUD_ENABLED",
        settings.egress.cloud_enabled.to_string(),
    );
    if let Ok(mut stt) = state.stt.lock() {
        stt.apply_settings(&settings.stt);
    }
    if let Ok(mut proactive) = state.proactive.lock() {
        proactive.apply_settings(settings.proactive.clone());
    }
    if let Ok(mut whitelist) = state.whitelist.lock() {
        let mut merged = Whitelist::default();
        for entry in settings.whitelist.clone() {
            merged.upsert(entry);
        }
        *whitelist = merged;
    }
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            "settings",
            "update",
            None,
            Some(true),
            "设置页保存（settings.json）",
        );
    }
    Ok(Json(json!({
        "ok": true,
        "note": "已写入 settings.json 并应用运行时设置（模型对新回合即时生效）",
    })))
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
    let action = request.action.clone();
    let entry = request.entry.clone();
    let app_id = request.app_id.clone();
    let entries = {
        let mut whitelist = state.whitelist.lock().map_err(poison)?;
        match action.as_str() {
            "upsert" => {
                let entry = entry
                    .clone()
                    .ok_or((StatusCode::BAD_REQUEST, "upsert 需要 entry".to_string()))?;
                whitelist.upsert(entry);
            }
            "remove" => {
                let app_id = app_id
                    .clone()
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
        whitelist.entries().to_vec()
    };
    let mut settings = owo_agent_core::Settings::load(&state.workspace);
    match action.as_str() {
        "upsert" => {
            let entry = entry.ok_or((StatusCode::BAD_REQUEST, "upsert 需要 entry".to_string()))?;
            if let Some(existing) = settings
                .whitelist
                .iter_mut()
                .find(|existing| existing.app_id == entry.app_id)
            {
                *existing = entry.clone();
            } else {
                settings.whitelist.push(entry);
            }
        }
        "remove" => {
            let app_id =
                app_id.ok_or((StatusCode::BAD_REQUEST, "remove 需要 app_id".to_string()))?;
            settings
                .whitelist
                .retain(|existing| existing.app_id != app_id);
        }
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("未知操作：{other}（upsert / remove）"),
            ))
        }
    }
    settings
        .save(&state.workspace)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    Ok(Json(entries))
}

struct ChannelApprover {
    pending: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<Decision>>>>,
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
        rx
    }
}

#[async_trait::async_trait]
impl Approver for ChannelApprover {
    async fn decide(&self, request: &PermissionRequest) -> Decision {
        if std::env::var("OWO_AUTO_APPROVE")
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
        {
            return Decision::Allow;
        }
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

/// P3 录制自动观察：录制中每 2s 采样前台应用/剪贴板事件（掩码）进入样本。
/// 前台应用变化只记一次，剪贴板变化按序列号去重。
pub async fn start_observer(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    let mut last_app: Option<(String, String)> = None;
    let mut last_clipboard: u32 = 0;
    loop {
        interval.tick().await;
        let (foreground, clipboard_changed) = {
            let mut perception = state
                .perception
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let _ = perception.refresh_from_platform();
            let sequence = owo_agent_core::clipboard_sequence();
            let changed = sequence != 0 && sequence != last_clipboard;
            perception.refresh_clipboard(sequence);
            let _ = perception.refresh_from_uia(2, 32);
            let snapshot = perception.snapshot();
            (snapshot.foreground_app.clone(), changed)
        };
        let mut pipeline = state
            .pipeline
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if pipeline.recorder.state() != LearnState::Recording {
            continue;
        }
        if let Some(app) = &foreground {
            let key = (app.id.clone(), app.title.clone());
            if last_app.as_ref() != Some(&key) {
                last_app = Some(key);
                let _ = pipeline.recorder.record(RecordedAction {
                    app_id: app.id.clone(),
                    anchor: SemanticAnchor {
                        app_id: Some(app.id.clone()),
                        role: None,
                        name: app.title.clone(),
                        parent: None,
                    },
                    action_type: ActionType::Shortcut,
                    value_masked: true,
                    sensitive: false,
                    at: chrono::Utc::now().to_rfc3339(),
                });
            }
        }
        if clipboard_changed {
            last_clipboard = owo_agent_core::clipboard_sequence();
            if let Some(app) = &foreground {
                let _ = pipeline.recorder.record(RecordedAction {
                    app_id: app.id.clone(),
                    anchor: SemanticAnchor {
                        app_id: Some(app.id.clone()),
                        role: None,
                        name: "剪贴板".to_string(),
                        parent: None,
                    },
                    action_type: ActionType::Inject,
                    value_masked: true,
                    sensitive: false,
                    at: chrono::Utc::now().to_rfc3339(),
                });
            }
        }
    }
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
