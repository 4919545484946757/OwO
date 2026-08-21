//! Lane D Part 1 契约测试：Goal/Plan 编排 HTTP API。
//!
//! 独立编译目标：`goal_api.rs` 不引用 crate::/super::，本文件用 #[path] 挂载。
//! 存储全部落在 tempfile 临时目录。

#[path = "../src/goal_api.rs"]
mod goal_api;

use owo_agent_core::gateway::ModelProvider;
use owo_agent_core::permissions::Policy;
use owo_agent_core::sqlite_store::SqliteSessionStore;
use owo_agent_core::tools::ToolRegistry;
use owo_agent_core::Agent;
use owo_agent_server::AppState;
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

/// 串行化修改 OPENAI_API_KEY 的测试（并行下 remove_var/set_var 会互相污染）。
static ENV_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

/// 无外部依赖的最小模型 Provider（任何模型调用即失败）。
struct IdleProvider;

#[async_trait::async_trait]
impl ModelProvider for IdleProvider {
    async fn complete(
        &self,
        _messages: &[owo_agent_core::ChatMessage],
        _tools: &[owo_agent_core::ToolSpec],
    ) -> Result<owo_agent_core::ModelOutput, String> {
        Err("IdleProvider 不应被调用".to_string())
    }
}

async fn test_state() -> (Arc<AppState>, tempfile::TempDir) {
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
    let state = Arc::new(AppState::new(
        agent,
        store,
        workspace.join("traces"),
        temp.path().to_path_buf(),
        workspace,
    ));
    (state, temp)
}

fn request(method: &str, path: &str, body: Option<&str>) -> axum::http::Request<axum::body::Body> {
    use axum::http::{header, Method, Request};
    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(path);
    if let Some(b) = body {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
        return builder.body(axum::body::Body::from(b.to_string())).unwrap();
    }
    builder.body(axum::body::Body::empty()).unwrap()
}

async fn call(app: &axum::Router, method: &str, path: &str, body: Option<&str>) -> (u16, Value) {
    let response = app
        .clone()
        .oneshot(request(method, path, body))
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// 创建 goal + 三并行+join 计划，返回 (app, goal_id)。
async fn setup_goal_with_plan(state: Arc<AppState>) -> (axum::Router, String) {
    let app = goal_api::router(state);
    let (status, created) = call(
        &app,
        "POST",
        "/goal",
        Some(r#"{"objective":"编排验收目标"}"#),
    )
    .await;
    assert_eq!(status, 201, "{created}");
    let goal_id = created["goal"]["id"].as_str().unwrap().to_string();
    (app, goal_id)
}

/// 轮询运行状态直到非 Running/Pending 或超时（404=尚未落盘，继续轮询）。
async fn wait_terminal(app: &axum::Router, goal_id: &str, timeout_ms: u64) -> (u16, Value) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        let (status, value) = call(app, "GET", &format!("/goal/{goal_id}/status"), None).await;
        if status != 404 {
            let goal_status = value
                .get("goal_status")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !matches!(
                goal_status,
                "Running" | "Pending" | "Planning" | "Verifying"
            ) {
                return (status, value);
            }
        }
        if std::time::Instant::now() > deadline {
            panic!("运行状态轮询超时：最后响应 {status} {value}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn create_goal_and_list() {
    let (state, _temp) = test_state().await;
    let app = goal_api::router(state);
    let (status, created) =
        call(&app, "POST", "/goal", Some(r#"{"objective":"创建与列表"}"#)).await;
    assert_eq!(status, 201);
    let goal_id = created["goal"]["id"].as_str().unwrap().to_string();
    let (status, list) = call(&app, "GET", "/goal", None).await;
    assert_eq!(status, 200);
    assert_eq!(list["count"].as_u64().unwrap(), 1);
    assert_eq!(list["goals"][0]["id"].as_str().unwrap(), goal_id);
}

#[tokio::test]
async fn create_goal_empty_objective_rejected() {
    let (state, _temp) = test_state().await;
    let app = goal_api::router(state);
    let (status, value) = call(&app, "POST", "/goal", Some(r#"{"objective":"  "}"#)).await;
    assert_eq!(status, 400);
    assert!(value["error"].as_str().unwrap().contains("objective"));
}

#[tokio::test]
async fn get_goal_not_found() {
    let (state, _temp) = test_state().await;
    let app = goal_api::router(state);
    let (status, value) = call(&app, "GET", "/goal/missing-goal", None).await;
    assert_eq!(status, 404);
    assert!(value["error"].as_str().unwrap().contains("不存在"));
}

#[tokio::test]
async fn plan_cycle_rejected_with_400() {
    let (state, _temp) = test_state().await;
    let (app, goal_id) = setup_goal_with_plan(state).await;
    let body = r#"{"steps":[
        {"id":"a","worker":"echo","deps":["b"]},
        {"id":"b","worker":"echo","deps":["a"]}
    ]}"#;
    let (status, value) = call(&app, "POST", &format!("/goal/{goal_id}/plan"), Some(body)).await;
    assert_eq!(status, 400);
    assert!(value["error"].as_str().unwrap().contains("环"), "{value}");
}

#[tokio::test]
async fn plan_waves_preview_for_parallel_join() {
    let (state, _temp) = test_state().await;
    let (app, goal_id) = setup_goal_with_plan(state).await;
    let body = r#"{"steps":[
        {"id":"a","worker":"echo","input":{"text":"A"}},
        {"id":"b","worker":"echo","input":{"text":"B"}},
        {"id":"c","worker":"echo","input":{"text":"C"}},
        {"id":"join","worker":"echo","deps":["a","b","c"],"input":{"text":"ABC"},"verify":"ABC"}
    ]}"#;
    let (status, value) = call(&app, "POST", &format!("/goal/{goal_id}/plan"), Some(body)).await;
    assert_eq!(status, 201, "{value}");
    let waves = value["waves"].as_array().unwrap();
    assert_eq!(waves.len(), 2);
    assert_eq!(waves[0].as_array().unwrap().len(), 3, "前三步应同层并行");
    assert_eq!(waves[1].as_array().unwrap().len(), 1);
    assert_eq!(waves[1][0], "join");
    assert_eq!(value["valid"], true);
}

#[tokio::test]
async fn run_echo_sleep_plan_to_succeeded() {
    let (state, _temp) = test_state().await;
    let (app, goal_id) = setup_goal_with_plan(state).await;
    let body = r#"{"steps":[
        {"id":"a","worker":"echo","input":{"text":"A"}},
        {"id":"b","worker":"echo","input":{"text":"B"}},
        {"id":"join","worker":"sleep","deps":["a","b"],"input":{"ms":20},"verify":"slept"}
    ]}"#;
    let (status, _) = call(&app, "POST", &format!("/goal/{goal_id}/plan"), Some(body)).await;
    assert_eq!(status, 201);
    let (status, run) = call(&app, "POST", &format!("/goal/{goal_id}/run"), Some("{}")).await;
    assert_eq!(status, 202, "{run}");
    let run_id = run["run_id"].as_str().unwrap().to_string();
    assert!(run_id.starts_with("run-"));

    let (status, value) = wait_terminal(&app, &goal_id, 10_000).await;
    assert_eq!(status, 200, "{value}");
    assert_eq!(value["goal_status"], "Succeeded", "{value}");
    let records = value["records"].as_object().unwrap();
    for (step_id, record) in records {
        let step_status = record["status"].as_str().unwrap();
        assert_eq!(step_status, "Succeeded", "步骤 {step_id} 应成功：{record}");
    }
    assert!(value["steps_taken"].as_u64().unwrap() >= 3);
    // runs 列表一致。
    let (_, runs) = call(&app, "GET", &format!("/goal/{goal_id}/runs"), None).await;
    assert_eq!(runs["count"].as_u64().unwrap(), 1);
    assert_eq!(runs["runs"][0]["run_id"].as_str().unwrap(), run_id);
}

#[tokio::test]
async fn fail_step_triggers_replan_and_fails_goal() {
    let (state, _temp) = test_state().await;
    let (app, goal_id) = setup_goal_with_plan(state).await;
    // join 验证恒失败 → 每次 replan 后仍失败，replan 次数用尽 → Failed。
    let body = r#"{"steps":[
        {"id":"a","worker":"echo","input":{"text":"A"}},
        {"id":"b","worker":"echo","input":{"text":"B"}},
        {"id":"join","worker":"echo","deps":["a","b"],"input":{"text":"X"},"verify":"NEVER-MATCH"}
    ]}"#;
    let (status, _) = call(&app, "POST", &format!("/goal/{goal_id}/plan"), Some(body)).await;
    assert_eq!(status, 201);
    let (status, _) = call(&app, "POST", &format!("/goal/{goal_id}/run"), Some("{}")).await;
    assert_eq!(status, 202);
    let (status, value) = wait_terminal(&app, &goal_id, 10_000).await;
    assert_eq!(status, 200, "{value}");
    assert_eq!(value["goal_status"], "Failed", "{value}");
    assert!(
        value["replan_count"].as_u64().unwrap() >= 1,
        "验证失败应触发 replan：{value}"
    );
    // 审计含 replan 记录。
    let (_, audit) = call(&app, "GET", &format!("/goal/{goal_id}/audit"), None).await;
    let text = serde_json::to_string(&audit).unwrap();
    assert!(text.contains("replan"), "审计应含 replan：{text}");
}

#[tokio::test]
async fn abort_marks_goal_aborted() {
    let (state, _temp) = test_state().await;
    let (app, goal_id) = setup_goal_with_plan(state).await;
    let body = r#"{"steps":[
        {"id":"slow","worker":"sleep","input":{"ms":5000}}
    ]}"#;
    let (status, _) = call(&app, "POST", &format!("/goal/{goal_id}/plan"), Some(body)).await;
    assert_eq!(status, 201);
    let (status, _) = call(&app, "POST", &format!("/goal/{goal_id}/run"), Some("{}")).await;
    assert_eq!(status, 202);
    // 立即 abort。
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let (status, value) = call(&app, "POST", &format!("/goal/{goal_id}/abort"), Some("{}")).await;
    assert_eq!(status, 200, "{value}");
    assert_eq!(value["aborted"].as_u64().unwrap(), 1);
    let (status, value) = wait_terminal(&app, &goal_id, 5_000).await;
    assert_eq!(status, 200, "{value}");
    assert_eq!(value["goal_status"], "Aborted", "{value}");
}

#[tokio::test]
async fn run_persists_recovery_state_consistent() {
    let (state, _temp) = test_state().await;
    let (app, goal_id) = setup_goal_with_plan(state).await;
    let body = r#"{"steps":[
        {"id":"a","worker":"echo","input":{"text":"A"}},
        {"id":"b","worker":"echo","deps":["a"],"input":{"text":"B"}}
    ]}"#;
    let (status, _) = call(&app, "POST", &format!("/goal/{goal_id}/plan"), Some(body)).await;
    assert_eq!(status, 201);
    let (status, _) = call(&app, "POST", &format!("/goal/{goal_id}/run"), Some("{}")).await;
    assert_eq!(status, 202);
    let (status, first) = wait_terminal(&app, &goal_id, 10_000).await;
    assert_eq!(first["goal_status"], "Succeeded");
    // 二次读取状态一致（幂等）。
    let (_, second) = call(&app, "GET", &format!("/goal/{goal_id}/status"), None).await;
    assert_eq!(first["goal_status"], second["goal_status"]);
    assert_eq!(first["records"], second["records"]);
    assert_eq!(status, 200);
}

#[tokio::test]
async fn unknown_goal_404_on_all_subresources() {
    let (state, _temp) = test_state().await;
    let app = goal_api::router(state);
    for (method, path, body) in [
        (
            "POST",
            "/goal/none/plan",
            Some(r#"{"steps":[{"id":"a","worker":"echo"}]}"#),
        ),
        ("POST", "/goal/none/run", Some("{}")),
        ("GET", "/goal/none/status", None),
        ("POST", "/goal/none/abort", Some("{}")),
        ("GET", "/goal/none/audit", None),
        ("GET", "/goal/none/runs", None),
    ] {
        let (status, value) = call(&app, method, path, body).await;
        assert_eq!(status, 404, "{method} {path} 应 404：{value}");
    }
}

#[tokio::test]
async fn audit_tail_records_write_operations() {
    let (state, _temp) = test_state().await;
    let (app, goal_id) = setup_goal_with_plan(state).await;
    let (_, audit) = call(&app, "GET", &format!("/goal/{goal_id}/audit"), None).await;
    let entries = audit["audit"].as_array().unwrap();
    assert!(!entries.is_empty());
    let text = serde_json::to_string(&audit).unwrap();
    assert!(text.contains("goal.create"), "审计应含创建记录");
}

#[tokio::test]
async fn plan_missing_404() {
    let (state, _temp) = test_state().await;
    let (app, goal_id) = setup_goal_with_plan(state).await;
    let (status, value) = call(&app, "GET", &format!("/goal/{goal_id}/plan"), None).await;
    assert_eq!(status, 404, "{value}");
}

// ==================== R5：agent worker / status 增强 ====================

/// 创建 goal + 计划（steps 由调用方提供），返回 (app, goal_id)。
async fn setup_goal_with_steps(state: Arc<AppState>, steps: &str) -> (axum::Router, String) {
    let (app, goal_id) = setup_goal_with_plan(state).await;
    let body = format!(r#"{{"steps":{steps}}}"#);
    let (status, value) = call(&app, "POST", &format!("/goal/{goal_id}/plan"), Some(&body)).await;
    assert_eq!(status, 201, "计划创建应成功：{value}");
    (app, goal_id)
}

fn agent_step_json(prompt: &str, read_only: bool) -> String {
    format!(
        r#"[{{"id":"a1","worker":"agent","input":{{"prompt":"{}","read_only":{}}},"verify":{{"kind":"nonempty"}}}}]"#,
        prompt, read_only
    )
}

#[tokio::test]
async fn r5_agent_worker_without_key_fails_readably() {
    // 保证无凭据：保存并移除 OPENAI_API_KEY
    let previous = std::env::var("OPENAI_API_KEY").ok();
    let _guard = ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    std::env::remove_var("OPENAI_API_KEY");
    let (state, _temp) = test_state().await;
    let steps = agent_step_json("做一个总结", true);
    let (app, goal_id) = setup_goal_with_steps(state.clone(), &steps).await;
    let (status, value) = call(
        &app,
        "POST",
        &format!("/goal/{goal_id}/run"),
        Some(r#"{"config":{}}"#),
    )
    .await;
    assert_eq!(status, 202, "{value}");
    let (_status, terminal) = wait_terminal(&app, &goal_id, 8000).await;
    let steps_value = terminal["steps"].as_array().unwrap();
    let a1 = steps_value.iter().find(|s| s["step_id"] == "a1").unwrap();
    let error = a1["error"].as_str().unwrap_or("");
    assert!(
        error.contains("OPENAI_API_KEY"),
        "无凭据时错误应可读且含 KEY 提示：{a1}"
    );
    // 恢复环境
    if let Some(key) = previous {
        std::env::set_var("OPENAI_API_KEY", key);
    }
}

#[tokio::test]
async fn r5_agent_step_missing_prompt_rejected_400() {
    let (state, _temp) = test_state().await;
    let (app, goal_id) = setup_goal_with_plan(state.clone()).await;
    let body = r#"{"steps":[{"id":"a1","worker":"agent","input":{"read_only":true}}]}"#;
    let (status, value) = call(&app, "POST", &format!("/goal/{goal_id}/plan"), Some(body)).await;
    assert_eq!(status, 400, "agent 步骤缺 prompt 应 400：{value}");
    assert!(value["error"].as_str().unwrap_or("").contains("prompt"));
}

#[tokio::test]
async fn r5_echo_sleep_fail_regression() {
    let (state, _temp) = test_state().await;
    let (app, goal_id) = setup_goal_with_steps(
        state.clone(),
        r#"[{"id":"e1","worker":"echo","input":{"text":"hello"}},
            {"id":"s1","worker":"sleep","input":{"ms":20},"deps":["e1"]},
            {"id":"f1","worker":"fail","input":{"text":"boom"},"deps":["s1"]}]"#,
    )
    .await;
    let (status, value) = call(
        &app,
        "POST",
        &format!("/goal/{goal_id}/run"),
        Some(r#"{"config":{}}"#),
    )
    .await;
    assert_eq!(status, 202, "{value}");
    let (_status, terminal) = wait_terminal(&app, &goal_id, 8000).await;
    assert_eq!(terminal["goal_status"], "Failed");
    let steps = terminal["steps"].as_array().unwrap();
    let e1 = steps.iter().find(|s| s["step_id"] == "e1").unwrap();
    assert_eq!(e1["output"], "hello", "{e1}");
    let f1 = steps.iter().find(|s| s["step_id"] == "f1").unwrap();
    assert!(f1["error"].as_str().unwrap_or("").contains("boom"));
}

#[tokio::test]
async fn r5_status_includes_worker_name_and_truncated_output() {
    let (state, _temp) = test_state().await;
    let long_text = "长文本".repeat(1000); // 3000 字符
    let steps = format!(
        r#"[{{"id":"e1","worker":"echo","input":{{"text":"{}"}}}}]"#,
        long_text
    );
    let (app, goal_id) = setup_goal_with_steps(state.clone(), &steps).await;
    let (status, value) = call(
        &app,
        "POST",
        &format!("/goal/{goal_id}/run"),
        Some(r#"{"config":{}}"#),
    )
    .await;
    assert_eq!(status, 202, "{value}");
    let (_status, terminal) = wait_terminal(&app, &goal_id, 8000).await;
    let steps_value = terminal["steps"].as_array().unwrap();
    let e1 = steps_value.iter().find(|s| s["step_id"] == "e1").unwrap();
    assert_eq!(e1["worker"], "echo", "status 应含 worker 名：{e1}");
    assert_eq!(e1["output_truncated"], true, "超过 2000 字符应截断");
    let output = e1["output"].as_str().unwrap();
    assert!(output.chars().count() <= 2000, "截断后不超过 2000");
}

#[tokio::test]
async fn r5_goal_plan_requires_agent_worker_validation_skips_other_workers() {
    let (state, _temp) = test_state().await;
    let (app, goal_id) = setup_goal_with_plan(state.clone()).await;
    // 非 agent worker 无需 prompt 校验
    let body = r#"{"steps":[{"id":"e1","worker":"echo","input":{}}]}"#;
    let (status, _) = call(&app, "POST", &format!("/goal/{goal_id}/plan"), Some(body)).await;
    assert_eq!(status, 201, "echo 步骤无需 prompt");
}

#[tokio::test]
async fn r5_agent_worker_online_skipped_without_key() {
    // 真实联网用例：仅当 OPENAI_API_KEY 且 OWO_AGENT_LIVE_TEST=1 才执行。
    // （scoped 门禁环境用 IdleProvider，真实模型调用必然失败；联调时显式开启双开关。）
    let has_key = std::env::var("OPENAI_API_KEY")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    let live = std::env::var("OWO_AGENT_LIVE_TEST")
        .map(|v| v == "1")
        .unwrap_or(false);
    if !(has_key && live) {
        return;
    }
    // 与 remove_var KEY 的测试串行（共享进程级环境变量）。
    let _guard = ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    // 模型选择：OWO_AGENT_MODEL > DeepSeek 端点适配 > 缺省（由 worker 决定）。
    let model = std::env::var("OWO_AGENT_MODEL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| {
            let base = std::env::var("OPENAI_BASE_URL").unwrap_or_default();
            if base.contains("deepseek") {
                "deepseek-chat".to_string()
            } else {
                "gpt-4.1-mini".to_string()
            }
        });
    let (state, _temp) = test_state().await;
    let steps = format!(
        r#"[{{"id":"a1","worker":"agent","input":{{"prompt":"回复 ok 即可","read_only":true,"model":"{model}"}}}}]"#
    );
    let (app, goal_id) = setup_goal_with_steps(state.clone(), &steps).await;
    let (status, value) = call(
        &app,
        "POST",
        &format!("/goal/{goal_id}/run"),
        Some(r#"{"config":{}}"#),
    )
    .await;
    assert_eq!(status, 202, "{value}");
    let (_status, terminal) = wait_terminal(&app, &goal_id, 60000).await;
    assert_eq!(terminal["goal_status"], "Succeeded");
}

#[tokio::test]
async fn r5_agent_worker_resolve_model_default() {
    // resolve_model 纯逻辑：无 OWO_AGENT_MODEL / input.model → 缺省值
    let input = serde_json::json!({ "prompt": "x" });
    let ok = goal_api::agent_worker::validate_agent_input(&input).is_ok();
    assert!(ok, "含 prompt 的 agent input 应通过预校验");
    assert_eq!(
        goal_api::agent_worker::AgentWorker::resolve_model(&input),
        "gpt-4.1-mini"
    );
    let input_model = serde_json::json!({ "prompt": "x", "model": "my-model" });
    assert_eq!(
        goal_api::agent_worker::AgentWorker::resolve_model(&input_model),
        "my-model"
    );
}

#[tokio::test]
async fn r5_status_steps_shape_complete() {
    // status.steps 每个条目应含 worker/status/attempts/output/output_truncated/error 字段。
    let (state, _temp) = test_state().await;
    let (app, goal_id) = setup_goal_with_steps(
        state.clone(),
        r#"[{"id":"e1","worker":"echo","input":{"text":"shape"}}]"#,
    )
    .await;
    let (status, _) = call(
        &app,
        "POST",
        &format!("/goal/{goal_id}/run"),
        Some(r#"{"config":{}}"#),
    )
    .await;
    assert_eq!(status, 202);
    let (_status, terminal) = wait_terminal(&app, &goal_id, 8000).await;
    let steps = terminal["steps"].as_array().unwrap();
    assert!(!steps.is_empty());
    let s = &steps[0];
    assert_eq!(s["worker"], "echo");
    assert!(s["status"].is_string());
    assert!(s["attempts"].is_number());
    assert!(s["output"].is_string());
    assert!(s["output_truncated"].is_boolean());
    assert!(
        s["error"].is_null() || s["error"].is_string(),
        "error 字段应存在：{s}"
    );
}
