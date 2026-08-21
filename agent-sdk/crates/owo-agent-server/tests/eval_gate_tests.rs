//! eval 护栏测试（R5 Agent 3 子任务 1）：/eval/gate/*。
//!
//! 独立编译：`#[path = "../src/eval_gate.rs"] mod eval_gate;`。
//! 环境变量（OPENAI_API_KEY / OPENAI_BASE_URL）用进程内锁串行化，用后恢复。

use owo_agent_core::permissions::Policy;
use owo_agent_core::sqlite_store::SqliteSessionStore;
use owo_agent_core::tools::ToolRegistry;
use owo_agent_core::Agent;
use std::sync::{Arc, OnceLock};
use tower::ServiceExt;

#[path = "../src/eval_gate.rs"]
mod eval_gate;

/// 环境变量互斥（避免并行测试互相污染进程级 env；tokio Mutex 可跨 await 持锁）。
fn env_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}
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

async fn send(
    state: Arc<owo_agent_server::AppState>,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> axum::http::Response<axum::body::Body> {
    eval_gate::router(state)
        .oneshot(request(method, path, body))
        .await
        .unwrap()
}

async fn body_json(response: axum::http::Response<axum::body::Body>) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// 保存/恢复环境变量的辅助。
struct EnvGuard(Vec<(String, Option<String>)>);

impl EnvGuard {
    fn save() -> Self {
        let keys = ["OPENAI_API_KEY", "OPENAI_BASE_URL", "OWO_AGENT_MODEL"];
        let saved = keys
            .iter()
            .map(|k| (k.to_string(), std::env::var(k).ok()))
            .collect();
        Self(saved)
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in &self.0 {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

#[tokio::test]
async fn run_without_credentials_returns_skipped() {
    let _guard = env_lock().lock().await;
    let _env = EnvGuard::save();
    std::env::remove_var("OPENAI_API_KEY");
    std::env::remove_var("OPENAI_BASE_URL");

    let (state, _temp) = test_state().await;
    let response = send(state, "POST", "/eval/gate/run", Some("{}")).await;
    assert_eq!(response.status().as_u16(), 200);
    let body = body_json(response).await;
    assert_eq!(body["skipped"], serde_json::json!(true), "无凭据应 skipped");
    assert!(
        body["reason"]
            .as_str()
            .unwrap_or("")
            .contains("OPENAI_API_KEY"),
        "reason 应说明缺凭据：{body}"
    );
}

#[tokio::test]
async fn run_with_dead_endpoint_persists_report_and_shape() {
    let _guard = env_lock().lock().await;
    let _env = EnvGuard::save();
    // 假凭据 + 指向 127.0.0.1:1（连接立即拒绝）→ 全 case 失败但报告仍落盘。
    std::env::set_var("OPENAI_API_KEY", "test-key-not-real");
    std::env::set_var("OPENAI_BASE_URL", "http://127.0.0.1:1/v1");

    let (state, temp) = test_state().await;
    // 迷你套件（1 case）→ 死端点下快速失败，报告仍落盘。
    let suite_path = temp.path().join("mini-suite.json");
    std::fs::write(
        &suite_path,
        r#"{"name":"mini","cases":[{"name":"c1","prompt":"hi"}]}"#,
    )
    .unwrap();
    let body = serde_json::json!({ "suite": suite_path.to_string_lossy() }).to_string();
    let response = send(state, "POST", "/eval/gate/run", Some(&body)).await;
    assert_eq!(response.status().as_u16(), 200, "有凭据不 skipped");
    let body = body_json(response).await;
    assert_eq!(body["ok"], serde_json::json!(true));
    let report = &body["report"];
    assert!(report["file"].as_str().unwrap_or("").ends_with(".json"));
    assert!(report["total"].as_u64().unwrap_or(0) > 0, "应有 case 数");
    assert!(report["passed"].as_u64().is_some());
    assert!(report["failures"].is_array());
    assert!(!report["model"].as_str().unwrap_or("").is_empty());

    // 落盘检查
    let reports_dir = temp.path().join("eval").join("reports");
    assert!(reports_dir.is_dir(), "报告目录应已创建");
    let files: Vec<_> = std::fs::read_dir(&reports_dir).unwrap().flatten().collect();
    assert_eq!(files.len(), 1, "应恰好落盘一份报告");
}

#[tokio::test]
async fn latest_report_404_when_empty() {
    let (state, _temp) = test_state().await;
    let response = send(state, "GET", "/eval/gate/report", None).await;
    assert_eq!(response.status().as_u16(), 404);
    let body = body_json(response).await;
    assert!(body["error"].as_str().is_some(), "404 应带 error 字段");
}

#[tokio::test]
async fn reports_history_empty_ok() {
    let (state, _temp) = test_state().await;
    let response = send(state, "GET", "/eval/gate/reports", None).await;
    assert_eq!(response.status().as_u16(), 200);
    let body = body_json(response).await;
    assert_eq!(body["count"], serde_json::json!(0));
    assert_eq!(body["reports"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn reports_history_newest_first() {
    let (state, temp) = test_state().await;
    let dir = temp.path().join("eval").join("reports");
    std::fs::create_dir_all(&dir).unwrap();
    let old = serde_json::json!({
        "file": "20260101T000000Z.json",
        "timestamp": "20260101T000000Z",
        "suite": "builtin",
        "total": 2, "passed": 1, "pass_rate": 0.5, "total_duration_ms": 10,
        "failures": [], "model": "m1",
    });
    let new = serde_json::json!({
        "file": "20260202T000000Z.json",
        "timestamp": "20260202T000000Z",
        "suite": "builtin",
        "total": 2, "passed": 2, "pass_rate": 1.0, "total_duration_ms": 20,
        "failures": [], "model": "m2",
    });
    std::fs::write(
        dir.join("20260101T000000Z.json"),
        serde_json::to_string(&old).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.join("20260202T000000Z.json"),
        serde_json::to_string(&new).unwrap(),
    )
    .unwrap();

    let response = send(state.clone(), "GET", "/eval/gate/reports", None).await;
    assert_eq!(response.status().as_u16(), 200);
    let body = body_json(response).await;
    assert_eq!(body["count"], serde_json::json!(2));
    let reports = body["reports"].as_array().unwrap();
    assert_eq!(
        reports[0]["file"],
        serde_json::json!("20260202T000000Z.json"),
        "最新在前"
    );
    assert_eq!(
        reports[1]["file"],
        serde_json::json!("20260101T000000Z.json")
    );

    let latest = send(state, "GET", "/eval/gate/report", None).await;
    assert_eq!(latest.status().as_u16(), 200);
    let latest_body = body_json(latest).await;
    assert_eq!(
        latest_body["file"],
        serde_json::json!("20260202T000000Z.json")
    );
}

#[tokio::test]
async fn run_with_bad_suite_path_returns_400() {
    let _guard = env_lock().lock().await;
    let _env = EnvGuard::save();
    std::env::set_var("OPENAI_API_KEY", "test-key-not-real");

    let (state, _temp) = test_state().await;
    let body = format!(r#"{{"suite":"{}"}}"#, "no-such-suite-file.json");
    let response = send(state, "POST", "/eval/gate/run", Some(&body)).await;
    assert_eq!(response.status().as_u16(), 400);
    let body = body_json(response).await;
    assert!(
        body["error"].as_str().unwrap_or("").contains("套件"),
        "{body}"
    );
}
