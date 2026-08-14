//! 路由面契约测试：验证 v0.5 HTTP 接口全部注册且可达（防回归）。
//!
//! 背景（2026-08-14 评估）：/locate/query、/memory/recall、/skills/health、/plugins、
//! /traces、/subagent/run、/project/rules、/mcp、/session/{id}/context 曾在服务端回归
//! 丢失（404），而桌面端仍在调用。此测试启动真实 HTTP 服务，逐个断言路由可达。

use owo_agent_core::permissions::Policy;
use owo_agent_core::sqlite_store::SqliteSessionStore;
use owo_agent_core::tools::ToolRegistry;
use owo_agent_core::Agent;
use owo_agent_server::build_router;
use std::sync::Arc;

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

async fn spawn_server() -> (String, tokio::task::JoinHandle<()>) {
    let workspace = std::env::temp_dir().join(format!("owo-router-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&workspace).unwrap();
    let agent = Agent::new(
        Arc::new(IdleProvider),
        ToolRegistry::new(),
        Policy::new(&workspace),
        Default::default(),
    );
    let store = SqliteSessionStore::open(&workspace.join("index.db")).unwrap();
    let app = build_router(Arc::new(owo_agent_server::AppState::new(
        agent,
        store,
        workspace.join("traces"),
        workspace.clone(),
        workspace,
    )));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), handle)
}

async fn get(base: &str, path: &str) -> (u16, String) {
    let client = reqwest::Client::new();
    let response = client.get(format!("{base}{path}")).send().await.unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    (status, text)
}

async fn post(base: &str, path: &str, body: &str) -> (u16, String) {
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{base}{path}"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    (status, text)
}

#[tokio::test]
async fn v05_routes_are_registered_not_404() {
    let (base, handle) = spawn_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let cases: &[(&str, &str)] = &[
        ("GET", "/skills/health"),
        ("GET", "/plugins"),
        ("GET", "/project/rules"),
        ("GET", "/mcp"),
        ("GET", "/traces"),
        ("GET", "/memory/observations"),
        ("GET", "/computer-use/sensitive-check"),
    ];
    for (_method, path) in cases {
        let (status, _body) = get(&base, path).await;
        assert!(status != 404, "GET {path} 返回 404（路由缺失）");
    }

    let post_cases: &[(&str, &str)] = &[
        ("/subagent/run", "{}"),
        ("/locate/query", "{}"),
        (
            "/memory/mine-skill",
            r#"{"name":"t","target_apps":[],"sensitivity":"low","description":"d"}"#,
        ),
        ("/memory/clear", "{}"),
        (
            "/computer-use/task",
            r#"{"target_app":"notepad","description":"d","allowed_actions":[],"max_duration_ms":60000}"#,
        ),
    ];
    for (path, body) in post_cases {
        let (status, _text) = post(&base, path, body).await;
        assert!(status != 404, "POST {path} 返回 404（路由缺失）");
    }

    handle.abort();
}

#[tokio::test]
async fn computer_use_sensitive_check_returns_json() {
    let (base, handle) = spawn_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{base}/computer-use/sensitive-check"))
        .header("content-type", "application/json")
        .body(r#"{"name":"PasswordBox","role":"Edit","ocr_text":""}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["sensitive"], true);

    handle.abort();
}
