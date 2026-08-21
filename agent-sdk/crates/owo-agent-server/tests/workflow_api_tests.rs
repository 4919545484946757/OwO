//! 工作流 HTTP API 契约测试（Lane C）。
//!
//! `#[path = "../src/workflow_api.rs"] mod workflow_api;` 独立编译；
//! 全部使用 tempfile 临时目录，绝不触碰真实 data_root/workspace。

#[path = "../src/workflow_api.rs"]
mod workflow_api;

use axum::body::Body;
use axum::http::{header, Method, Request, Response, StatusCode};
use owo_agent_core::permissions::Policy;
use owo_agent_core::sqlite_store::SqliteSessionStore;
use owo_agent_core::tools::ToolRegistry;
use owo_agent_core::workflow::ActionBackend;
use owo_agent_core::Agent;
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;

/// 无外部依赖的最小模型 Provider（任何模型调用即失败）。
struct IdleProvider;

#[async_trait::async_trait]
impl owo_agent_core::gateway::ModelProvider for IdleProvider {
    async fn complete(
        &self,
        _messages: &[owo_agent_core::ChatMessage],
        _tools: &[owo_agent_core::ToolSpec],
    ) -> Result<owo_agent_core::ModelOutput, String> {
        Err("IdleProvider 不应被调用".to_string())
    }
}

async fn test_state() -> (Arc<owo_agent_server::AppState>, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let agent = Agent::new(
        Arc::new(IdleProvider),
        ToolRegistry::new(),
        Policy::new(&workspace),
        Default::default(),
    );
    let store = SqliteSessionStore::open(&workspace.join("index.db")).unwrap();
    let state = Arc::new(owo_agent_server::AppState::new(
        agent,
        store,
        workspace.join("traces"),
        temp.path().to_path_buf(),
        workspace,
    ));
    (state, temp)
}

fn request(method: &str, path: &str, body: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(path);
    if let Some(b) = body {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
        return builder.body(Body::from(b.to_string())).unwrap();
    }
    builder.body(Body::empty()).unwrap()
}

async fn send(
    state: Arc<owo_agent_server::AppState>,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Response<Body> {
    workflow_api::router(state)
        .oneshot(request(method, path, body))
        .await
        .unwrap()
}

async fn body_json(response: Response<Body>) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

const TRIGGER_MANUAL: &str = r#"{"id": "t1", "kind": {"kind": "manual"}}"#;

/// json! 宏内的 triggers 字面量（json! 不支持引用字符串常量展开）。
fn trigger_manual_json() -> serde_json::Value {
    serde_json::from_str(TRIGGER_MANUAL).unwrap()
}

fn sample_flow() -> serde_json::Value {
    json!({
        "id": "demo", "name": "demo-flow", "version": 1,
        "triggers": [trigger_manual_json()],
        "permissions": [{"scope": "fs.write", "mode": "allow"}],
        "preconditions": [],
        "rollback_points": [],
        "max_steps": 100, "subflow_depth_limit": 5,
        "steps": [
            {"kind": "act", "id": "w1", "scope": "fs.write",
             "spec": {"action": "write_file", "target": "a.txt", "value": "hello"}},
            {"kind": "assert", "id": "a1", "expr": "exists(w1)", "timeout_ms": 100},
            {"kind": "notify", "id": "n1", "message": "done"}
        ]
    })
}

fn rollback_flow() -> serde_json::Value {
    json!({
        "id": "rb", "name": "rb-flow", "version": 1,
        "triggers": [trigger_manual_json()],
        "permissions": [{"scope": "fs.write", "mode": "allow"}],
        "preconditions": [],
        "rollback_points": [],
        "max_steps": 100, "subflow_depth_limit": 5,
        "steps": [
            {"kind": "act", "id": "s1", "scope": "fs.write",
             "spec": {"action": "write_file", "target": "b.txt", "value": "keep"}},
            {"kind": "rollback_point", "id": "cp1"},
            {"kind": "act", "id": "s2", "scope": "fs.write",
             "spec": {"action": "write_file", "target": "c.txt", "value": "temp"}},
            {"kind": "assert", "id": "a1", "expr": "false", "timeout_ms": 100}
        ]
    })
}

fn precondition_flow() -> serde_json::Value {
    json!({
        "id": "pre", "name": "pre-flow", "version": 1,
        "triggers": [trigger_manual_json()],
        "permissions": [{"scope": "fs.write", "mode": "allow"}],
        "preconditions": ["ready == true"],
        "rollback_points": [],
        "max_steps": 100, "subflow_depth_limit": 5,
        "steps": [
            {"kind": "act", "id": "w1", "scope": "fs.write",
             "spec": {"action": "write_file", "target": "a.txt", "value": "x"}}
        ]
    })
}

fn invalid_flow() -> serde_json::Value {
    json!({
        "id": "bad", "name": "bad-flow", "version": 1,
        "triggers": [trigger_manual_json()],
        "permissions": [],
        "preconditions": [],
        "rollback_points": [],
        "max_steps": 100, "subflow_depth_limit": 5,
        "steps": []
    })
}

fn write_flow(dir: &std::path::Path, name: &str, flow: &serde_json::Value) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join(format!("{name}.owflow")),
        serde_json::to_string_pretty(flow).unwrap(),
    )
    .unwrap();
}

/// 轮询 run 快照直到终态（或超时）。
async fn poll_run(state: Arc<owo_agent_server::AppState>, run_id: &str) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let response = send(
            state.clone(),
            "GET",
            &format!("/workflow/run/{run_id}"),
            None,
        )
        .await;
        let snapshot = body_json(response).await;
        let state_str = snapshot["state"].as_str().unwrap_or("running").to_string();
        if state_str != "running" {
            return snapshot;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("等待 run {run_id} 终态超时：{snapshot}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

// ---------------------------------------------------------------------------
// 发现与加载
// ---------------------------------------------------------------------------

#[tokio::test]
async fn discover_lists_owflow_files_only() {
    let (state, temp) = test_state().await;
    let ws = state.workspace.clone();
    write_flow(&ws, "a", &sample_flow());
    write_flow(&ws.join("sub"), "b", &sample_flow());
    std::fs::write(ws.join("notes.txt"), "x").unwrap();

    let response = send(state, "GET", "/workflow", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let value = body_json(response).await;
    let names: Vec<&str> = value["flows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"a"), "应发现 a.owflow：{names:?}");
    assert!(names.contains(&"b"), "应发现子目录 b.owflow：{names:?}");
    assert!(
        !names.iter().any(|n| n.contains("notes")),
        "非 owflow 文件不应出现"
    );
    drop(temp);
}

#[tokio::test]
async fn get_flow_valid_definition() {
    let (state, temp) = test_state().await;
    write_flow(&state.workspace.clone(), "demo", &sample_flow());
    let response = send(state, "GET", "/workflow/demo", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let value = body_json(response).await;
    assert_eq!(value["valid"], json!(true));
    assert_eq!(value["definition"]["id"], json!("demo"));
    assert_eq!(value["definition"]["steps"].as_array().unwrap().len(), 3);
    drop(temp);
}

#[tokio::test]
async fn get_flow_invalid_dsl_reports_issues() {
    let (state, temp) = test_state().await;
    write_flow(&state.workspace.clone(), "bad", &invalid_flow());
    let response = send(state, "GET", "/workflow/bad", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let value = body_json(response).await;
    assert_eq!(value["valid"], json!(false));
    assert!(!value["issues"].as_array().unwrap().is_empty());
    drop(temp);
}

#[tokio::test]
async fn get_flow_unknown_returns_404() {
    let (state, temp) = test_state().await;
    let response = send(state, "GET", "/workflow/nope", None).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let value = body_json(response).await;
    assert!(value["error"].as_str().unwrap().contains("不存在"));
    drop(temp);
}

// ---------------------------------------------------------------------------
// 内联校验
// ---------------------------------------------------------------------------

#[tokio::test]
async fn validate_inline_accepts_valid_and_rejects_bad() {
    let (state, temp) = test_state().await;
    let response = send(
        state.clone(),
        "POST",
        "/workflow/validate",
        Some(&sample_flow().to_string()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let value = body_json(response).await;
    assert_eq!(value["valid"], json!(true));

    let response = send(
        state,
        "POST",
        "/workflow/validate",
        Some(&invalid_flow().to_string()),
    )
    .await;
    let value = body_json(response).await;
    assert_eq!(value["valid"], json!(false));
    assert!(!value["issues"].as_array().unwrap().is_empty());
    drop(temp);
}

#[tokio::test]
async fn validate_inline_malformed_json_returns_400() {
    let (state, temp) = test_state().await;
    let response = send(state, "POST", "/workflow/validate", Some("{not json")).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    drop(temp);
}

// ---------------------------------------------------------------------------
// 运行
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_succeeds_and_snapshot_has_steps() {
    let (state, temp) = test_state().await;
    write_flow(&state.workspace.clone(), "demo", &sample_flow());
    let response = send(state.clone(), "POST", "/workflow/demo/run", Some("{}")).await;
    let status = response.status();
    if status != StatusCode::CREATED {
        let text = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        panic!("run 返回 {status}：{}", String::from_utf8_lossy(&text));
    }
    let run_id = body_json(response).await["run_id"]
        .as_str()
        .unwrap()
        .to_string();

    let snapshot = poll_run(state.clone(), &run_id).await;
    assert_eq!(snapshot["state"], json!("succeeded"));
    let steps = snapshot["steps"].as_array().unwrap();
    assert!(!steps.is_empty(), "步骤日志不应为空");
    let kinds: Vec<&str> = steps.iter().map(|s| s["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, vec!["act", "assert", "notify"]);
    // 引擎写入了 a.txt（MockBackend 沙箱在 data_root/workflow-runs/<run_id>/）
    let work_root = temp.path().join("workflow-runs").join(&run_id);
    assert_eq!(
        std::fs::read_to_string(work_root.join("a.txt")).unwrap(),
        "hello"
    );
    drop(temp);
}

#[tokio::test]
async fn run_outcome_persisted_to_disk() {
    let (state, temp) = test_state().await;
    write_flow(&state.workspace.clone(), "demo", &sample_flow());
    let response = send(state.clone(), "POST", "/workflow/demo/run", Some("{}")).await;
    let run_id = body_json(response).await["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    poll_run(state.clone(), &run_id).await;

    let runs_dir = temp.path().join("workflow-runs").join(&run_id);
    let outcome: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(runs_dir.join("outcome.json")).unwrap())
            .unwrap();
    assert_eq!(outcome["state"], json!("succeeded"));
    assert!(runs_dir.join("audit.json").exists());
    assert!(runs_dir.join("meta.json").exists());
    drop(temp);
}

#[tokio::test]
async fn rollback_to_nearest_checkpoint_on_failure() {
    let (state, temp) = test_state().await;
    write_flow(&state.workspace.clone(), "rb", &rollback_flow());
    let response = send(state.clone(), "POST", "/workflow/rb/run", Some("{}")).await;
    let run_id = body_json(response).await["run_id"]
        .as_str()
        .unwrap()
        .to_string();

    let snapshot = poll_run(state.clone(), &run_id).await;
    assert_eq!(snapshot["state"], json!("failed"));
    assert_eq!(
        snapshot["rollback_to"],
        json!("cp1"),
        "失败应回滚到最近检查点"
    );
    // 检查点之后写入的 c.txt 被回滚删除；b.txt 保留
    let work_root = temp.path().join("workflow-runs").join(&run_id);
    assert!(!work_root.join("c.txt").exists(), "检查点后的文件应被回滚");
    assert_eq!(
        std::fs::read_to_string(work_root.join("b.txt")).unwrap(),
        "keep"
    );
    drop(temp);
}

#[tokio::test]
async fn precondition_failed_stops_before_any_step() {
    let (state, temp) = test_state().await;
    write_flow(&state.workspace.clone(), "pre", &precondition_flow());
    let response = send(state.clone(), "POST", "/workflow/pre/run", Some("{}")).await;
    let run_id = body_json(response).await["run_id"]
        .as_str()
        .unwrap()
        .to_string();

    let snapshot = poll_run(state.clone(), &run_id).await;
    assert_eq!(snapshot["state"], json!("failed"));
    assert!(snapshot["steps"].as_array().unwrap().is_empty());
    drop(temp);
}

#[tokio::test]
async fn abort_marks_aborted() {
    let (state, temp) = test_state().await;
    write_flow(&state.workspace.clone(), "demo", &sample_flow());
    let response = send(state.clone(), "POST", "/workflow/demo/run", Some("{}")).await;
    let run_id = body_json(response).await["run_id"]
        .as_str()
        .unwrap()
        .to_string();

    // 20ms 启动窗口内立即 abort（try_lock 拿到锁 → engine.abort()）
    let response = send(
        state.clone(),
        "POST",
        &format!("/workflow/run/{run_id}/abort"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let value = body_json(response).await;
    assert_eq!(value["abort_requested"], json!(true));

    let snapshot = poll_run(state.clone(), &run_id).await;
    assert_eq!(snapshot["state"], json!("aborted"));
    drop(temp);
}

#[tokio::test]
async fn run_unknown_flow_returns_404() {
    let (state, temp) = test_state().await;
    let response = send(state, "POST", "/workflow/nope/run", Some("{}")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    drop(temp);
}

// ---------------------------------------------------------------------------
// runs 列表 / 快照 / 审计
// ---------------------------------------------------------------------------

#[tokio::test]
async fn runs_list_contains_run() {
    let (state, temp) = test_state().await;
    write_flow(&state.workspace.clone(), "demo", &sample_flow());
    let response = send(state.clone(), "POST", "/workflow/demo/run", Some("{}")).await;
    let run_id = body_json(response).await["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    poll_run(state.clone(), &run_id).await;

    let response = send(state.clone(), "GET", "/workflow/demo/runs", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let value = body_json(response).await;
    let runs = value["runs"].as_array().unwrap();
    assert!(
        runs.iter()
            .any(|r| r["run_id"].as_str() == Some(run_id.as_str())),
        "runs 列表应包含本次运行：{value}"
    );
    let matched = runs
        .iter()
        .find(|r| r["run_id"].as_str() == Some(run_id.as_str()))
        .unwrap();
    assert_eq!(matched["state"], json!("succeeded"));
    drop(temp);
}

#[tokio::test]
async fn run_snapshot_unknown_returns_404() {
    let (state, temp) = test_state().await;
    let response = send(state, "GET", "/workflow/run/does-not-exist", None).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let value = body_json(response).await;
    assert!(value["error"].as_str().unwrap().contains("不存在"));
    drop(temp);
}

#[tokio::test]
async fn abort_unknown_run_returns_404() {
    let (state, temp) = test_state().await;
    let response = send(state, "POST", "/workflow/run/does-not-exist/abort", None).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    drop(temp);
}

#[tokio::test]
async fn run_audit_tail_nonempty() {
    let (state, temp) = test_state().await;
    write_flow(&state.workspace.clone(), "demo", &sample_flow());
    let response = send(state.clone(), "POST", "/workflow/demo/run", Some("{}")).await;
    let run_id = body_json(response).await["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    poll_run(state.clone(), &run_id).await;

    let response = send(
        state.clone(),
        "GET",
        &format!("/workflow/run/{run_id}/audit"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let value = body_json(response).await;
    let audit = value["audit"].as_array().unwrap();
    assert!(!audit.is_empty(), "审计尾部不应为空");
    assert!(
        audit.iter().any(|e| e["event"]
            .as_str()
            .map(|s| s.contains("workflow"))
            .unwrap_or(false)),
        "审计应含 workflow 事件：{value}"
    );
    drop(temp);
}

#[tokio::test]
async fn run_with_ctx_recorded_in_snapshot() {
    let (state, temp) = test_state().await;
    write_flow(&state.workspace.clone(), "demo", &sample_flow());
    let response = send(
        state.clone(),
        "POST",
        "/workflow/demo/run",
        Some(r#"{"ctx": {"note": "from-api"}}"#),
    )
    .await;
    let run_id = body_json(response).await["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let snapshot = poll_run(state.clone(), &run_id).await;
    assert_eq!(snapshot["ctx"]["note"], json!("from-api"));
    drop(temp);
}

// ---------------------------------------------------------------------------
// 独立编译冒烟：模块可被 #[path] 加载（本文件顶部已声明 mod workflow_api）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn router_builds_with_state() {
    let (state, temp) = test_state().await;
    let router = workflow_api::router(state);
    // 构建不 panic 即通过
    let _ = router;
    drop(temp);
}

// ==================== R5：真实后端 / 人审 / SSE ====================

/// 人审流程：act 写文件 → 人审 → notify。
fn approval_flow() -> serde_json::Value {
    json!({
        "id": "appr", "name": "appr-flow", "version": 1,
        "triggers": [trigger_manual_json()],
        "permissions": [{"scope": "fs.write", "mode": "allow"}],
        "preconditions": [],
        "rollback_points": [],
        "max_steps": 100, "subflow_depth_limit": 5,
        "steps": [
            {"kind": "act", "id": "w1", "scope": "fs.write",
             "spec": {"action": "write_file", "target": "a.txt", "value": "pre"}},
            {"kind": "human_approve", "id": "h1", "prompt": "是否继续？"},
            {"kind": "notify", "id": "n1", "message": "done"}
        ]
    })
}

/// 高危桌面动作流程（真实后端应门禁拒绝）。
fn gated_action_flow() -> serde_json::Value {
    json!({
        "id": "gate", "name": "gate-flow", "version": 1,
        "triggers": [trigger_manual_json()],
        "permissions": [{"scope": "ui.operate", "mode": "allow"}],
        "preconditions": [],
        "rollback_points": [],
        "max_steps": 100, "subflow_depth_limit": 5,
        "steps": [
            {"kind": "act", "id": "g1", "scope": "ui.operate",
             "spec": {"action": "launch", "target": "notepad"}}
        ]
    })
}

/// 越界写文件流程（真实后端应拒绝）。
fn escape_flow() -> serde_json::Value {
    json!({
        "id": "esc", "name": "esc-flow", "version": 1,
        "triggers": [trigger_manual_json()],
        "permissions": [{"scope": "fs.write", "mode": "allow"}],
        "preconditions": [],
        "rollback_points": [],
        "max_steps": 100, "subflow_depth_limit": 5,
        "steps": [
            {"kind": "act", "id": "e1", "scope": "fs.write",
             "spec": {"action": "write_file", "target": "../escape.txt", "value": "x"}}
        ]
    })
}

/// 感知流程（真实后端 foreground）。
fn sense_flow(target: &str) -> serde_json::Value {
    json!({
        "id": "sense", "name": "sense-flow", "version": 1,
        "triggers": [trigger_manual_json()],
        "permissions": [],
        "preconditions": [],
        "rollback_points": [],
        "max_steps": 100, "subflow_depth_limit": 5,
        "steps": [
            {"kind": "sense", "id": "s1", "spec": {"target": target}}
        ]
    })
}

/// 启动 run 并轮询到终态（超时 8s）。
async fn run_to_terminal(
    state: Arc<owo_agent_server::AppState>,
    name: &str,
    body: &str,
) -> (String, serde_json::Value) {
    let response = send(
        state.clone(),
        "POST",
        &format!("/workflow/{name}/run"),
        Some(body),
    )
    .await;
    let created = body_json(response).await;
    assert!(created["run_id"].as_str().is_some(), "{created}");
    let run_id = created["run_id"].as_str().unwrap().to_string();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        let snapshot = body_json(
            send(
                state.clone(),
                "GET",
                &format!("/workflow/run/{run_id}"),
                None,
            )
            .await,
        )
        .await;
        let state_name = snapshot["state"].as_str().unwrap_or("running").to_string();
        if state_name != "running" && state_name != "waiting_approval" {
            return (run_id, snapshot);
        }
        assert!(std::time::Instant::now() < deadline, "运行超时：{run_id}");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn r5_real_backend_gated_actions_denied() {
    let (state, temp) = test_state().await;
    write_flow(&temp.path().join("ws"), "gate-flow", &gated_action_flow());
    let (run_id, snapshot) =
        run_to_terminal(state.clone(), "gate-flow", r#"{"backend":"real"}"#).await;
    assert_eq!(
        snapshot["state"], "failed",
        "桌面动作应被门禁拒绝：{snapshot}"
    );
    // 错误可读性：审计 workflow.failed 条目应含门禁说明
    let audit = body_json(
        send(
            state.clone(),
            "GET",
            &format!("/workflow/run/{run_id}/audit"),
            None,
        )
        .await,
    )
    .await;
    let text = serde_json::to_string(&audit).unwrap_or_default();
    assert!(text.contains("门禁"), "审计应含门禁说明：{text}");
}

#[tokio::test]
async fn r5_real_backend_write_file_inside_workspace() {
    let (state, temp) = test_state().await;
    write_flow(&temp.path().join("ws"), "demo-flow", &sample_flow());
    let (_run_id, snapshot) =
        run_to_terminal(state.clone(), "demo-flow", r#"{"backend":"real"}"#).await;
    assert_eq!(snapshot["state"], "succeeded", "{snapshot}");
    // 真实后端 write_file 写入 workspace
    let content = std::fs::read_to_string(temp.path().join("ws").join("a.txt")).unwrap();
    assert_eq!(content, "hello");
}

#[tokio::test]
async fn r5_real_backend_outside_workspace_rejected() {
    let (state, temp) = test_state().await;
    write_flow(&temp.path().join("ws"), "esc-flow", &escape_flow());
    let (run_id, snapshot) =
        run_to_terminal(state.clone(), "esc-flow", r#"{"backend":"real"}"#).await;
    assert_eq!(snapshot["state"], "failed", "越界写应失败：{snapshot}");
    let audit = body_json(
        send(
            state.clone(),
            "GET",
            &format!("/workflow/run/{run_id}/audit"),
            None,
        )
        .await,
    )
    .await;
    let text = serde_json::to_string(&audit).unwrap_or_default();
    assert!(text.contains("边界"), "审计应含越界说明：{text}");
    assert!(!temp.path().join("escape.txt").exists(), "越界文件不得创建");
}

#[tokio::test]
async fn r5_real_backend_sense_foreground() {
    let (state, temp) = test_state().await;
    write_flow(
        &temp.path().join("ws"),
        "sense-flow",
        &sense_flow("foreground"),
    );
    let (_run_id, snapshot) =
        run_to_terminal(state.clone(), "sense-flow", r#"{"backend":"real"}"#).await;
    assert_eq!(snapshot["state"], "succeeded", "{snapshot}");
}

#[tokio::test]
async fn r5_real_backend_unknown_sense_target_fails() {
    let (state, temp) = test_state().await;
    write_flow(
        &temp.path().join("ws"),
        "sense-flow",
        &sense_flow("nonsense"),
    );
    let (_run_id, snapshot) =
        run_to_terminal(state.clone(), "sense-flow", r#"{"backend":"real"}"#).await;
    assert_eq!(snapshot["state"], "failed", "{snapshot}");
}

#[tokio::test]
async fn r5_real_backend_mcp_unregistered_fails_readably() {
    let (state, temp) = test_state().await;
    let flow = json!({
        "id": "mcp", "name": "mcp-flow", "version": 1,
        "triggers": [trigger_manual_json()],
        "permissions": [{"scope": "mcp.call", "mode": "allow"}],
        "preconditions": [], "rollback_points": [],
        "max_steps": 100, "subflow_depth_limit": 5,
        "steps": [
            {"kind": "invoke_mcp", "id": "m1", "scope": "mcp.call", "server": "no-such-server", "tool": "ping", "args": {}}
        ]
    });
    write_flow(&temp.path().join("ws"), "mcp-flow", &flow);
    let (run_id, snapshot) =
        run_to_terminal(state.clone(), "mcp-flow", r#"{"backend":"real"}"#).await;
    assert_eq!(snapshot["state"], "failed", "{snapshot}");
    let audit = body_json(
        send(
            state.clone(),
            "GET",
            &format!("/workflow/run/{run_id}/audit"),
            None,
        )
        .await,
    )
    .await;
    let text = serde_json::to_string(&audit).unwrap_or_default();
    assert!(text.contains("MCP"), "审计应含 MCP 错误：{text}");
}

#[tokio::test]
async fn r5_real_backend_missing_skill_fails_readably() {
    let (state, temp) = test_state().await;
    let flow = json!({
        "id": "sk", "name": "sk-flow", "version": 1,
        "triggers": [trigger_manual_json()],
        "permissions": [{"scope": "skill.call", "mode": "allow"}],
        "preconditions": [], "rollback_points": [],
        "max_steps": 100, "subflow_depth_limit": 5,
        "steps": [
            {"kind": "invoke_skill", "id": "k1", "skill": "no-such-skill", "args": {}}
        ]
    });
    write_flow(&temp.path().join("ws"), "sk-flow", &flow);
    let (_run_id, snapshot) =
        run_to_terminal(state.clone(), "sk-flow", r#"{"backend":"real"}"#).await;
    assert_eq!(snapshot["state"], "failed", "{snapshot}");
}

#[tokio::test]
async fn r5_approval_approve_path_continues() {
    let (state, temp) = test_state().await;
    write_flow(&temp.path().join("ws"), "appr-flow", &approval_flow());
    let response = send(
        state.clone(),
        "POST",
        "/workflow/appr-flow/run",
        Some(r#"{"backend":"real","approval_timeout_ms":5000}"#),
    )
    .await;
    let created = body_json(response).await;
    let run_id = created["run_id"].as_str().unwrap().to_string();

    // 等待 WaitingApproval
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let snapshot = body_json(
            send(
                state.clone(),
                "GET",
                &format!("/workflow/run/{run_id}"),
                None,
            )
            .await,
        )
        .await;
        let state_name = snapshot["state"].as_str().unwrap_or("running").to_string();
        if state_name == "waiting_approval" {
            assert!(
                snapshot["pending_approval"]["prompt"].is_string(),
                "{snapshot}"
            );
            assert_eq!(snapshot["pending_approval"]["run_id"], run_id);
            break;
        }
        if state_name != "running" {
            panic!("意外终态：{state_name} {snapshot}");
        }
        assert!(std::time::Instant::now() < deadline, "等待审批超时");
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }

    // 批准
    let resp = send(
        state.clone(),
        "POST",
        &format!("/workflow/run/{run_id}/approval"),
        Some(r#"{"decision":"approve"}"#),
    )
    .await;
    assert_eq!(resp.status().as_u16(), 200);

    // 轮询原 run_id → Succeeded（含 notify）
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let snap = body_json(
            send(
                state.clone(),
                "GET",
                &format!("/workflow/run/{run_id}"),
                None,
            )
            .await,
        )
        .await;
        let state_name = snap["state"].as_str().unwrap_or("running").to_string();
        if state_name == "succeeded" {
            break;
        }
        if state_name == "failed" {
            panic!("审批通过后应成功：{snap}");
        }
        assert!(std::time::Instant::now() < deadline, "等待成功超时");
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }
}

#[tokio::test]
async fn r5_approval_reject_path_fails() {
    let (state, temp) = test_state().await;
    write_flow(&temp.path().join("ws"), "appr-flow", &approval_flow());
    let response = send(
        state.clone(),
        "POST",
        "/workflow/appr-flow/run",
        Some(r#"{"backend":"real","approval_timeout_ms":5000}"#),
    )
    .await;
    let created = body_json(response).await;
    let run_id = created["run_id"].as_str().unwrap().to_string();

    // 等到 WaitingApproval
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let snapshot = body_json(
            send(
                state.clone(),
                "GET",
                &format!("/workflow/run/{run_id}"),
                None,
            )
            .await,
        )
        .await;
        if snapshot["state"].as_str() == Some("waiting_approval") {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "等待审批超时");
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }
    let resp = send(
        state.clone(),
        "POST",
        &format!("/workflow/run/{run_id}/approval"),
        Some(r#"{"decision":"reject"}"#),
    )
    .await;
    assert_eq!(resp.status().as_u16(), 200);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let snap = body_json(
            send(
                state.clone(),
                "GET",
                &format!("/workflow/run/{run_id}"),
                None,
            )
            .await,
        )
        .await;
        let state_name = snap["state"].as_str().unwrap_or("running").to_string();
        if state_name == "failed" || state_name == "aborted" {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "等待拒绝终态超时：{snap}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }
}

#[tokio::test]
async fn r5_approval_timeout_rejects() {
    let (state, temp) = test_state().await;
    write_flow(&temp.path().join("ws"), "appr-flow", &approval_flow());
    let (_run_id, snapshot) = run_to_terminal(
        state.clone(),
        "appr-flow",
        r#"{"backend":"real","approval_timeout_ms":300}"#,
    )
    .await;
    assert_eq!(snapshot["state"], "failed", "超时应拒绝：{snapshot}");
}

#[tokio::test]
async fn r5_approval_error_paths() {
    let (state, temp) = test_state().await;
    write_flow(&temp.path().join("ws"), "appr-flow", &approval_flow());
    // 未知 run → 404
    let resp = send(
        state.clone(),
        "POST",
        "/workflow/run/no-such/approval",
        Some(r#"{"decision":"approve"}"#),
    )
    .await;
    assert_eq!(resp.status().as_u16(), 404);
    // 非法 decision → 400
    let resp = send(
        state.clone(),
        "POST",
        "/workflow/run/no-such/approval",
        Some(r#"{"decision":"maybe"}"#),
    )
    .await;
    assert_eq!(resp.status().as_u16(), 400);
    let _ = temp;
}

#[tokio::test]
async fn r5_sse_events_stream_contains_frames() {
    let (state, temp) = test_state().await;
    write_flow(&temp.path().join("ws"), "demo-flow", &sample_flow());
    // 真实端口（SSE 流不结束，oneshot 读 body 会挂起）
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = workflow_api::router(state.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/workflow/demo-flow/run"))
        .header("content-type", "application/json")
        .body(r#"{"backend":"mock"}"#)
        .send()
        .await
        .unwrap();
    let created: serde_json::Value = response.json().await.unwrap();
    let run_id = created["run_id"].as_str().unwrap().to_string();

    // 订阅 SSE：断言 content-type + 首帧含 event:/data:
    let mut stream = client
        .get(format!("{base}/workflow/run/{run_id}/events"))
        .send()
        .await
        .unwrap();
    assert!(
        stream
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .contains("text/event-stream"),
        "应返回 SSE content-type"
    );
    let first = tokio::time::timeout(std::time::Duration::from_secs(5), stream.chunk())
        .await
        .expect("SSE 首帧超时")
        .unwrap()
        .expect("SSE 流不应结束");
    let text = String::from_utf8_lossy(&first).to_string();
    assert!(text.contains("event:"), "SSE 应含 event 行：{text}");
    assert!(text.contains("data:"), "SSE 应含 data 行：{text}");
}

#[tokio::test]
async fn r5_mock_backend_default_still_works() {
    let (state, temp) = test_state().await;
    write_flow(&temp.path().join("ws"), "demo-flow", &sample_flow());
    let (_run_id, snapshot) = run_to_terminal(state.clone(), "demo-flow", "{}").await;
    assert_eq!(snapshot["state"], "succeeded", "{snapshot}");
}

#[tokio::test]
async fn r5_mock_backend_keeps_original_semantics() {
    let (state, temp) = test_state().await;
    write_flow(&temp.path().join("ws"), "rb-flow", &rollback_flow());
    let (_run_id, snapshot) =
        run_to_terminal(state.clone(), "rb-flow", r#"{"backend":"mock"}"#).await;
    assert_eq!(snapshot["state"], "failed");
    assert_eq!(snapshot["rollback_to"], "cp1");
}

// ==================== R5：真实后端可注入桩（不触网） ====================

#[tokio::test]
async fn r5_real_backend_act_stub_injected() {
    let (state, _temp) = test_state().await;
    // 直接构造真实后端 + 桩：act 由桩决定（不触网、不动桌面）。
    let backend = workflow_api::workflow_backend::ServerActionBackend::new(state.clone())
        .with_act_stub(|spec: &owo_agent_core::workflow::ActSpec| {
            Ok(serde_json::json!({ "stubbed": true, "action": spec.action }))
        });
    let spec = owo_agent_core::workflow::ActSpec {
        action: "launch".into(),
        target: "notepad".into(),
        value: None,
    };
    let result = backend
        .with_act_stub(|_| Ok(serde_json::json!({ "stubbed": true })))
        .act(&spec)
        .await;
    assert!(result.is_ok(), "桩应放行：{result:?}");
    assert_eq!(result.unwrap()["stubbed"], true);
    // 无桩时桌面动作门禁拒绝
    let mut plain = workflow_api::workflow_backend::ServerActionBackend::new(state);
    let result = plain.act(&spec).await;
    assert!(result.is_err(), "无桩时 launch 应被门禁拒绝");
    let err = result.unwrap_err();
    assert!(err.contains("门禁"), "错误应说明门禁：{err}");
}
