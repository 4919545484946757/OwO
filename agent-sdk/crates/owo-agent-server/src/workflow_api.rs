//! 工作流 HTTP API（Lane C，.owflow v1 接入层；R5 扩展：真实后端 / 人审 / run 级 SSE）。
//!
//! 路由（前缀 /workflow）：
//!   GET  /workflow                          发现 workspace 下 *.owflow（深度上限 3）
//!   GET  /workflow/{name}                   加载 + 校验，返回 {definition, valid, issues}
//!   POST /workflow/validate                 内联定义校验
//!   POST /workflow/{name}/run {ctx?, backend?, approval_timeout_ms?} → 201 {run_id}
//!   GET  /workflow/{name}/runs              run 列表（注册表 + 落盘扫描）
//!   GET  /workflow/run/{run_id}             运行快照 {state, steps, rollback_to, error, pending_approval}
//!   POST /workflow/run/{run_id}/abort       中止
//!   GET  /workflow/run/{run_id}/audit       审计尾部
//!   POST /workflow/run/{run_id}/approval    {decision: "approve"|"reject"} 人审裁决
//!   GET  /workflow/run/{run_id}/events      run 级 SSE（历史重放 + 实时）
//!
//! 运行态：模块内 OnceLock 注册表（run_id → RunEntry）；engine 由 tokio::spawn 持有；
//! 执行工作目录 data_root/workflow-runs/<run_id>/；结果落盘 outcome.json + audit.json。
//! 后端选择：mock（MockBackend 沙箱，默认）/ real（ServerActionBackend + 真实人审 ChannelApprover）。
//!
//! 协议约束：本模块不使用 crate::/super::；AppState 全限定名 owo_agent_server::AppState；
//! 错误统一 (StatusCode, Json({error}))；不给 AppState 加字段。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Json;
use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use owo_agent_core::workflow::{
    AutoApprover, MockBackend, WorkflowDefinition, WorkflowEngine, WorkflowOutcome,
};

// R5 扩展：真实后端 / 人审 / run 级 SSE。
// workflow_backend 作为本模块子模块编译（lib.rs 无需登记；两文件独立编译、无 crate 引用）。
#[path = "workflow_backend.rs"]
pub mod workflow_backend;

use workflow_backend::{
    decide_approval, pending_approvals, BackendChoice, ChannelApprover, EventBackend,
    ServerActionBackend, WfEvents,
};

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<serde_json::Value>)>;

fn api_err(
    status: StatusCode,
    message: impl Into<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    (status, Json(serde_json::json!({ "error": message.into() })))
}

/// 运行条目：engine（tokio Mutex 供 abort）+ 结果快照 + 审计尾部 + 事件流。
pub struct RunEntry {
    pub run_id: String,
    pub name: String,
    pub engine: Arc<tokio::sync::Mutex<WorkflowEngine>>,
    pub abort_requested: Arc<AtomicBool>,
    pub outcome: Mutex<Option<serde_json::Value>>,
    pub audit_tail: Mutex<Vec<serde_json::Value>>,
    pub created_at: String,
    pub ctx: serde_json::Value,
    /// run 级 SSE 事件源（R5）。
    pub events: Arc<WfEvents>,
}

type RunRegistry = Arc<Mutex<HashMap<String, Arc<RunEntry>>>>;

fn runs_registry() -> RunRegistry {
    static RUNS: OnceLock<RunRegistry> = OnceLock::new();
    RUNS.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
        .clone()
}

const MAX_DISCOVER_DEPTH: usize = 3;

/// 递归发现 workspace 下的 .owflow 文件（深度受限）。
fn discover_flows(root: &FsPath, max_depth: usize) -> Vec<PathBuf> {
    fn walk(dir: &FsPath, depth: usize, max_depth: usize, out: &mut Vec<PathBuf>) {
        if depth > max_depth {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, depth + 1, max_depth, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("owflow") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, 0, max_depth, &mut out);
    out
}

/// 按 name（文件 stem）在工作区查找 .owflow。
fn find_flow(workspace: &FsPath, name: &str) -> Option<PathBuf> {
    discover_flows(workspace, MAX_DISCOVER_DEPTH)
        .into_iter()
        .find(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy() == name)
                .unwrap_or(false)
        })
}

fn load_flow(
    workspace: &FsPath,
    name: &str,
) -> Result<WorkflowDefinition, (StatusCode, Json<serde_json::Value>)> {
    let path = find_flow(workspace, name)
        .ok_or_else(|| api_err(StatusCode::NOT_FOUND, format!("工作流不存在：{name}")))?;
    let content = std::fs::read_to_string(&path)
        .map_err(|e| api_err(StatusCode::BAD_REQUEST, format!("读取 {name} 失败：{e}")))?;
    serde_json::from_str(&content)
        .map_err(|e| api_err(StatusCode::BAD_REQUEST, format!("解析 {name} 失败：{e}")))
}

fn outcome_snapshot(outcome: &WorkflowOutcome) -> serde_json::Value {
    serde_json::json!({
        "state": serde_json::to_value(outcome.state)
            .unwrap_or_else(|_| serde_json::json!("unknown")),
        "steps": serde_json::to_value(&outcome.steps)
            .unwrap_or_else(|_| serde_json::json!([])),
        "rollback_to": outcome.rollback_to,
    })
}

fn audit_tail_json(audit: &owo_agent_core::AuditLog, limit: usize) -> Vec<serde_json::Value> {
    audit
        .entries
        .iter()
        .rev()
        .take(limit)
        .filter_map(|entry| serde_json::to_value(entry).ok())
        .collect()
}

fn new_run_id() -> String {
    format!(
        "wf-{}-{}",
        chrono::Utc::now().format("%Y%m%d%H%M%S"),
        uuid::Uuid::new_v4().simple()
    )
}

/// 工作流 API 路由（由 lib.rs build_router 合并）。
pub fn router(state: Arc<owo_agent_server::AppState>) -> axum::Router {
    axum::Router::new()
        .route("/workflow", get(list_flows))
        .route("/workflow/validate", post(validate_definition_endpoint))
        .route("/workflow/{name}", get(get_flow))
        .route("/workflow/{name}/run", post(run_workflow))
        .route("/workflow/{name}/runs", get(list_runs))
        .route("/workflow/run/{run_id}", get(run_snapshot))
        .route("/workflow/run/{run_id}/abort", post(abort_run))
        .route("/workflow/run/{run_id}/audit", get(run_audit))
        .route("/workflow/run/{run_id}/approval", post(decide_run_approval))
        .route("/workflow/run/{run_id}/events", get(run_events_sse))
        .with_state(state)
}

async fn list_flows(
    State(state): State<Arc<owo_agent_server::AppState>>,
) -> ApiResult<serde_json::Value> {
    let workspace = state.workspace.clone();
    let flows: Vec<serde_json::Value> = discover_flows(&workspace, MAX_DISCOVER_DEPTH)
        .into_iter()
        .filter_map(|path| {
            let name = path.file_stem()?.to_string_lossy().to_string();
            let rel = path
                .strip_prefix(&workspace)
                .ok()?
                .to_string_lossy()
                .to_string();
            Some(serde_json::json!({ "name": name, "path": rel }))
        })
        .collect();
    Ok(Json(serde_json::json!({ "flows": flows })))
}

async fn get_flow(
    State(state): State<Arc<owo_agent_server::AppState>>,
    Path(name): Path<String>,
) -> ApiResult<serde_json::Value> {
    let flow = load_flow(&state.workspace, &name)?;
    let definition = serde_json::to_value(&flow).map_err(|e| {
        api_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("序列化定义失败：{e}"),
        )
    })?;
    match owo_agent_core::workflow::validate_definition(&flow, &[]) {
        Ok(()) => Ok(Json(
            serde_json::json!({ "definition": definition, "valid": true, "issues": [] }),
        )),
        Err(issues) => Ok(Json(
            serde_json::json!({ "definition": definition, "valid": false, "issues": issues }),
        )),
    }
}

async fn validate_definition_endpoint(
    Json(flow): Json<WorkflowDefinition>,
) -> ApiResult<serde_json::Value> {
    match owo_agent_core::workflow::validate_definition(&flow, &[]) {
        Ok(()) => Ok(Json(serde_json::json!({ "valid": true, "issues": [] }))),
        Err(issues) => Ok(Json(
            serde_json::json!({ "valid": false, "issues": issues }),
        )),
    }
}

#[derive(serde::Deserialize)]
struct RunRequest {
    #[serde(default)]
    ctx: serde_json::Map<String, serde_json::Value>,
    /// 执行后端："mock"（默认，沙箱）| "real"（真实后端，桌面动作门禁拒绝）。
    #[serde(default)]
    backend: Option<String>,
    /// 人审超时（毫秒；缺省 120s）。
    #[serde(default)]
    approval_timeout_ms: Option<u64>,
}

async fn run_workflow(
    State(state): State<Arc<owo_agent_server::AppState>>,
    Path(name): Path<String>,
    Json(request): Json<RunRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let flow = load_flow(&state.workspace, &name)?;
    let run_id = new_run_id();
    let runs_dir = state.data_root.join("workflow-runs").join(&run_id);
    std::fs::create_dir_all(&runs_dir).map_err(|e| {
        api_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("创建运行目录失败：{e}"),
        )
    })?;
    let ctx_value = serde_json::Value::Object(request.ctx.clone());
    let backend_choice = BackendChoice::parse(request.backend.as_deref());
    let events = Arc::new(WfEvents::new(128));

    // 后端：mock 沙箱或真实后端（真实后端 act 桌面动作门禁拒绝，人审走 oneshot 通道）。
    let engine = {
        let health = owo_agent_core::skill_health::SkillHealthStore::new(None);
        match backend_choice {
            BackendChoice::Real => {
                let backend = ServerActionBackend::new(state.clone());
                let wrapped = EventBackend::new(backend, events.clone(), run_id.clone());
                let timeout = std::time::Duration::from_millis(
                    request.approval_timeout_ms.unwrap_or(120_000),
                );
                WorkflowEngine::new(
                    flow.clone(),
                    HashMap::new(),
                    Box::new(wrapped),
                    Box::new(
                        ChannelApprover::new(run_id.clone(), timeout).with_events(events.clone()),
                    ),
                    health,
                    runs_dir.clone(),
                )
            }
            BackendChoice::Mock => {
                let wrapped = EventBackend::new(
                    MockBackend::new(runs_dir.clone()),
                    events.clone(),
                    run_id.clone(),
                );
                WorkflowEngine::new(
                    flow.clone(),
                    HashMap::new(),
                    Box::new(wrapped),
                    Box::new(AutoApprover { approve: true }),
                    health,
                    runs_dir.clone(),
                )
            }
        }
    };
    let entry = Arc::new(RunEntry {
        run_id: run_id.clone(),
        name: name.clone(),
        engine: Arc::new(tokio::sync::Mutex::new(engine)),
        abort_requested: Arc::new(AtomicBool::new(false)),
        outcome: Mutex::new(None),
        audit_tail: Mutex::new(Vec::new()),
        created_at: chrono::Utc::now().to_rfc3339(),
        ctx: ctx_value,
        events,
    });
    runs_registry()
        .lock()
        .unwrap()
        .insert(run_id.clone(), entry.clone());

    // meta.json：供 /workflow/{name}/runs 落盘扫描
    let _ = std::fs::write(
        runs_dir.join("meta.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "run_id": run_id, "name": name, "created_at": entry.created_at, "ctx": entry.ctx,
            "backend": if backend_choice == BackendChoice::Real { "real" } else { "mock" },
        }))
        .unwrap_or_default(),
    );

    let spawned = entry.clone();
    let run_dir = runs_dir.clone();
    tokio::spawn(async move {
        // 20ms 窗口：让 abort 请求有机会先置位（abort 通过同一把 tokio Mutex 调 abort()）。
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let mut guard = spawned.engine.lock().await;
        if spawned.abort_requested.load(Ordering::SeqCst) {
            guard.abort();
        }
        match guard.run().await {
            Ok(outcome) => {
                let snapshot = outcome_snapshot(&outcome);
                let audit = audit_tail_json(guard.audit(), 50);
                *spawned.outcome.lock().unwrap() = Some(snapshot.clone());
                *spawned.audit_tail.lock().unwrap() = audit.clone();
                spawned.events.push(
                    "state_change",
                    &serde_json::json!({ "run_id": spawned.run_id, "state": snapshot["state"] }),
                );
                if let Some(rollback_to) = snapshot.get("rollback_to") {
                    if !rollback_to.is_null() {
                        spawned.events.push(
                            "rollback",
                            &serde_json::json!({
                                "run_id": spawned.run_id,
                                "rollback_to": rollback_to,
                            }),
                        );
                    }
                }
                let _ = std::fs::write(
                    run_dir.join("outcome.json"),
                    serde_json::to_string_pretty(&snapshot).unwrap_or_default(),
                );
                let _ = std::fs::write(
                    run_dir.join("audit.json"),
                    serde_json::to_string_pretty(&serde_json::json!({ "audit": audit }))
                        .unwrap_or_default(),
                );
            }
            Err(error) => {
                let failed = serde_json::json!({ "state": "error", "error": error });
                *spawned.outcome.lock().unwrap() = Some(failed.clone());
                spawned.events.push(
                    "state_change",
                    &serde_json::json!({ "run_id": spawned.run_id, "state": "error" }),
                );
                let _ = std::fs::write(
                    run_dir.join("outcome.json"),
                    serde_json::to_string_pretty(&failed).unwrap_or_default(),
                );
            }
        }
    });

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "run_id": run_id })),
    ))
}

async fn list_runs(
    State(state): State<Arc<owo_agent_server::AppState>>,
    Path(name): Path<String>,
) -> ApiResult<serde_json::Value> {
    let registry = runs_registry();
    let mut runs: Vec<serde_json::Value> = Vec::new();
    // 注册表（含运行中）
    for entry in registry.lock().unwrap().values() {
        if entry.name != name {
            continue;
        }
        let outcome = entry.outcome.lock().unwrap().clone();
        let state = match &outcome {
            Some(value) => value["state"].clone(),
            None => serde_json::json!("running"),
        };
        runs.push(serde_json::json!({
            "run_id": entry.run_id,
            "name": entry.name,
            "created_at": entry.created_at,
            "state": state,
        }));
    }
    // 落盘扫描（进程重启后的历史 run）
    let runs_dir = state.data_root.join("workflow-runs");
    if let Ok(entries) = std::fs::read_dir(&runs_dir) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let meta_path = dir.join("meta.json");
            let Ok(content) = std::fs::read_to_string(&meta_path) else {
                continue;
            };
            let Ok(meta) = serde_json::from_str::<serde_json::Value>(&content) else {
                continue;
            };
            if meta.get("name").and_then(|v| v.as_str()) != Some(name.as_str()) {
                continue;
            }
            let run_id = meta
                .get("run_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if runs
                .iter()
                .any(|r| r["run_id"].as_str() == Some(run_id.as_str()))
            {
                continue;
            }
            let outcome_file = dir.join("outcome.json");
            let state = std::fs::read_to_string(&outcome_file)
                .ok()
                .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
                .and_then(|v| v["state"].clone().as_str().map(str::to_string))
                .unwrap_or_else(|| "running".to_string());
            runs.push(serde_json::json!({
                "run_id": run_id,
                "name": meta.get("name").cloned().unwrap_or_default(),
                "created_at": meta.get("created_at").cloned().unwrap_or_default(),
                "state": state,
            }));
        }
    }
    runs.sort_by(|a, b| b["created_at"].as_str().cmp(&a["created_at"].as_str()));
    Ok(Json(serde_json::json!({ "runs": runs })))
}

async fn run_snapshot(Path(run_id): Path<String>) -> ApiResult<serde_json::Value> {
    let entry = runs_registry()
        .lock()
        .unwrap()
        .get(&run_id)
        .cloned()
        .ok_or_else(|| api_err(StatusCode::NOT_FOUND, format!("运行不存在：{run_id}")))?;
    let outcome = entry.outcome.lock().unwrap().clone();
    let pending = pending_approvals(&run_id);
    // 人审等待中：引擎处于 WaitingApproval（outcome 尚未落盘），以 pending 判定。
    let state = if !pending.is_empty() {
        serde_json::json!("waiting_approval")
    } else {
        match &outcome {
            Some(value) => value["state"].clone(),
            None => serde_json::json!("running"),
        }
    };
    let steps = match &outcome {
        Some(value) => value["steps"].clone(),
        None => serde_json::json!([]),
    };
    let rollback_to = outcome
        .as_ref()
        .and_then(|value| value["rollback_to"].clone().as_str().map(str::to_string));
    // 失败原因：从审计尾部提取 workflow.failed 的 detail（引擎错误文本）。
    let error = entry
        .audit_tail
        .lock()
        .unwrap()
        .iter()
        .find(|a| a["event"].as_str() == Some("workflow.failed"))
        .and_then(|a| a["detail"].as_str())
        .map(str::to_string);
    Ok(Json(serde_json::json!({
        "run_id": entry.run_id,
        "name": entry.name,
        "state": state,
        "steps": steps,
        "rollback_to": rollback_to,
        "error": error,
        "created_at": entry.created_at,
        "ctx": entry.ctx,
        "outcome": outcome,
        "pending_approval": if pending.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::to_value(&pending[0]).unwrap_or(serde_json::Value::Null)
        },
    })))
}

#[derive(serde::Deserialize)]
struct ApprovalRequest {
    decision: String,
}

/// 人审裁决：POST /workflow/run/{run_id}/approval {decision: "approve"|"reject"}。
async fn decide_run_approval(
    Path(run_id): Path<String>,
    Json(request): Json<ApprovalRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let approve = match request.decision.as_str() {
        "approve" => true,
        "reject" => false,
        other => {
            return Err(api_err(
                StatusCode::BAD_REQUEST,
                format!("decision 只能为 approve|reject：{other}"),
            ))
        }
    };
    decide_approval(&run_id, approve).map_err(|e| api_err(StatusCode::NOT_FOUND, e))?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "run_id": run_id,
        "decision": if approve { "approve" } else { "reject" },
    })))
}

/// run 级 SSE 事件流：GET /workflow/run/{run_id}/events（历史重放 + 实时）。
async fn run_events_sse(
    Path(run_id): Path<String>,
) -> Result<
    axum::response::Sse<
        impl tokio_stream::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>,
    >,
    (StatusCode, Json<serde_json::Value>),
> {
    use tokio_stream::StreamExt;
    let entry = runs_registry()
        .lock()
        .unwrap()
        .get(&run_id)
        .cloned()
        .ok_or_else(|| api_err(StatusCode::NOT_FOUND, format!("运行不存在：{run_id}")))?;
    let history = entry.events.history();
    let receiver = entry.events.subscribe();
    let stream = tokio_stream::iter(
        history
            .into_iter()
            .map(|frame| Ok(axum::response::sse::Event::default().data(frame))),
    )
    .chain(
        tokio_stream::wrappers::BroadcastStream::new(receiver)
            .map(|item| Ok(axum::response::sse::Event::default().data(item.unwrap_or_default()))),
    );
    Ok(axum::response::Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new().interval(std::time::Duration::from_secs(15)),
    ))
}

async fn abort_run(Path(run_id): Path<String>) -> ApiResult<serde_json::Value> {
    let entry = runs_registry()
        .lock()
        .unwrap()
        .get(&run_id)
        .cloned()
        .ok_or_else(|| api_err(StatusCode::NOT_FOUND, format!("运行不存在：{run_id}")))?;
    entry.abort_requested.store(true, Ordering::SeqCst);
    // 若 run 尚未开始（20ms 窗口内）直接拿到锁 → 立即 abort；已开始则等待终态。
    if let Ok(mut guard) = entry.engine.try_lock() {
        guard.abort();
    }
    Ok(Json(
        serde_json::json!({ "run_id": run_id, "abort_requested": true }),
    ))
}

async fn run_audit(Path(run_id): Path<String>) -> ApiResult<serde_json::Value> {
    let entry = runs_registry()
        .lock()
        .unwrap()
        .get(&run_id)
        .cloned()
        .ok_or_else(|| api_err(StatusCode::NOT_FOUND, format!("运行不存在：{run_id}")))?;
    let tail = entry.audit_tail.lock().unwrap().clone();
    Ok(Json(serde_json::json!({ "run_id": run_id, "audit": tail })))
}
