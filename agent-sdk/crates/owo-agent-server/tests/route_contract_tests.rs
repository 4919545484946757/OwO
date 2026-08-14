//! 路由面契约测试：验证 v0.5/v0.6 HTTP 接口全部注册且可达（防回归）。
//!
//! 背景（2026-08-14）：/locate/query、/memory/recall、/skills/health、/plugins、
//! /traces、/subagent/run、/project/rules、/mcp、/session/{id}/context 曾在服务端回归
//! 丢失（404），而桌面端仍在调用。
//!
//! 本文件以 `clients/ts/openapi.json`（2026-08-13 权威契约快照）为基准：
//! 1. 断言每个契约路径 + 方法可到达（非 404/405，合法输入 2xx、最小非法输入 400/422 亦可）。
//! 2. 断言 `/openapi.json` 登记了契约快照全部路径与 lib.rs 实际注册的全部路由。
//! 3. 真实 HTTP 服务启动 smoke（防 Router 构建 panic）。

use owo_agent_core::permissions::Policy;
use owo_agent_core::sqlite_store::SqliteSessionStore;
use owo_agent_core::tools::ToolRegistry;
use owo_agent_core::Agent;
use owo_agent_server::build_router;
use std::sync::Arc;
use tower::ServiceExt;

/// 契约快照（权威）：agent-sdk/clients/ts/openapi.json。
const CONTRACT_SNAPSHOT: &str = include_str!("../../../clients/ts/openapi.json");

/// lib.rs 源码（用于提取实际注册路由，验证 openapi_spec 无漏登）。
const LIB_RS: &str = include_str!("../src/lib.rs");

/// 返回契约快照中的全部 (path, [method...])。
fn contract_endpoints() -> Vec<(String, Vec<String>)> {
    let snapshot = CONTRACT_SNAPSHOT.trim_start_matches('\u{feff}');
    let json: serde_json::Value = serde_json::from_str(snapshot).unwrap();
    json["paths"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(path, methods)| {
            let list = methods
                .as_object()
                .unwrap()
                .keys()
                .map(|m| m.to_uppercase())
                .collect();
            (path.clone(), list)
        })
        .collect()
}

/// 从 lib.rs 提取实际注册的路由路径。
fn registered_routes() -> Vec<String> {
    let mut routes = Vec::new();
    for line in LIB_RS.lines() {
        if let Some(start) = line.find(".route(") {
            let rest = &line[start + ".route(".len()..];
            if let Some(quote) = rest.find('"') {
                let after = &rest[quote + 1..];
                if let Some(end) = after.find('"') {
                    routes.push(after[..end].to_string());
                }
            }
        }
    }
    routes.sort();
    routes.dedup();
    routes
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

/// 契约路径的最小合法/非法请求体（缺省 {}，避免 route 存在性判断被 body 校验干扰）。
fn sample_body(path: &str) -> Option<&'static str> {
    match path {
        "/session" => Some(r#"{"workspace":".","model":"idle"}"#),
        "/session/{id}/turn" => Some(r#"{"prompt":"hi"}"#),
        "/session/{id}/permission/{request_id}" => Some(r#"{"approved":true}"#),
        "/plugins/{id}/enabled" => Some(r#"{"enabled":false}"#),
        "/subagent/run" => Some(r#"{"prompt":"hi","read_only":true}"#),
        "/project/rules" => Some(r#"{"content":"rules"}"#),
        "/mcp/add" => {
            Some(r#"{"name":"__missing__","transport":"http","url":"http://127.0.0.1:1"}"#)
        }
        "/mcp/remove" => Some(r#"{"name":"__missing__"}"#),
        "/locate/query" => Some(r#"{}"#),
        "/memory/mine-skill" => {
            Some(r#"{"name":"t","target_apps":[],"sensitivity":"low","description":"d"}"#)
        }
        "/skills/{name}/enabled" => Some(r#"{"enabled":false}"#),
        "/computer-use/task" => Some(r#"{"target_app":"notepad","description":"d"}"#),
        "/computer-use/task/{id}/{action}" => Some(r#"{"reason":"r"}"#),
        "/computer-use/sensitive-check" => Some(r#"{"name":"PasswordBox"}"#),
        _ => Some(r#"{}"#),
    }
}

/// 占位路径参数（资源不存在时应返回 400/422/500，而不是 404——404 意味着路由缺失）。
fn sample_path(path: &str, session_id: &str) -> String {
    path.replace("{id}", session_id)
        .replace("{request_id}", "no-such-request")
        .replace("{name}", "no-such-name")
        .replace("{index}", "0")
        .replace("{app_id}", "no-such-app")
        .replace("{format}", "md")
        .replace("{action}", "cancel")
}

/// 资源型 404 白名单：路由已注册且方法匹配，但目标资源不存在时 handler 正确地返回 404。
/// 此类路径的契约断言为「非 405」+（非 404 或 404 由资源缺失产生）。
fn resource_404_ok(path: &str) -> bool {
    matches!(
        path,
        "/skills/{name}"
            | "/skills/{name}/enabled"
            | "/learn/packages/{name}"
            | "/learn/export/{name}"
            | "/traces/{index}"
            | "/mcp/remove"
            | "/automations/{id}/toggle"
            | "/perception/template/{app_id}"
            | "/computer-use/task/{id}/run"
            | "/cloud/tasks/{id}"
            | "/cloud/tasks/{id}/result"
    )
}

#[tokio::test]
async fn all_contract_endpoints_are_reachable() {
    let (state, _temp) = test_state().await;
    let app = build_router(state);

    // 创建真实会话，使 /session/{id}/* 系列走真实资源路径（强断言非 404）。
    let create_resp = app
        .clone()
        .oneshot(request(
            "POST",
            "/session",
            Some(r#"{"workspace":".","model":"idle"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(create_resp.status().as_u16(), 200, "POST /session 应 200");
    let create_bytes = axum::body::to_bytes(create_resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let session_id = serde_json::from_slice::<serde_json::Value>(&create_bytes)
        .unwrap()
        .get("id")
        .and_then(serde_json::Value::as_str)
        .expect("create_session 响应应含 id")
        .to_string();

    let mut failed: Vec<String> = Vec::new();
    for (path, methods) in contract_endpoints() {
        for method in methods {
            let is_get_like = matches!(method.as_str(), "GET" | "DELETE");
            let body = if is_get_like {
                None
            } else {
                Some(sample_body(&path).unwrap_or("{}"))
            };
            let uri = sample_path(&path, &session_id);
            let response = app
                .clone()
                .oneshot(request(&method, &uri, body))
                .await
                .unwrap();
            let status = response.status().as_u16();
            let ok = if status == 405 {
                false
            } else if status == 404 {
                resource_404_ok(&path)
            } else {
                true
            };
            if !ok {
                failed.push(format!("{method} {path} → {status}"));
            }
        }
    }
    assert!(
        failed.is_empty(),
        "契约路径不可达（404/405）：\n{}",
        failed.join("\n")
    );
}

#[tokio::test]
async fn openapi_json_covers_snapshot_and_registered_routes() {
    let (state, _temp) = test_state().await;
    let app = build_router(state);
    let response = app
        .oneshot(request("GET", "/openapi.json", None))
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200, "/openapi.json 应可访问");
    let bytes = axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap();
    let served: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let served_paths: Vec<&str> = served["paths"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();

    // 1) 契约快照全部路径已登记
    let mut missing: Vec<String> = Vec::new();
    for (path, _) in contract_endpoints() {
        if !served_paths.contains(&path.as_str()) {
            missing.push(path.clone());
        }
    }
    assert!(
        missing.is_empty(),
        "契约快照路径漏登记 /openapi.json：{missing:?}"
    );

    // 2) lib.rs 实际注册的路由全部已登记
    let mut missing_routes: Vec<String> = Vec::new();
    for route in registered_routes() {
        if !served_paths.contains(&route.as_str()) {
            missing_routes.push(route);
        }
    }
    assert!(
        missing_routes.is_empty(),
        "已注册路由漏登记 /openapi.json：{missing_routes:?}"
    );
}

#[tokio::test]
async fn v05_routes_are_registered_not_404_via_real_http() {
    let (state, _temp) = test_state().await;
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let base = format!("http://{addr}");

    let client = reqwest::Client::new();
    let get_cases = [
        "/health",
        "/usage",
        "/skills/health",
        "/plugins",
        "/project/rules",
        "/mcp",
        "/traces",
        "/memory/observations",
        "/memory/recall?q=t&top_k=3",
        "/openapi.json",
    ];
    for path in get_cases {
        let status = client
            .get(format!("{base}{path}"))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16();
        assert!(
            status != 404 && status != 405,
            "GET {path} → {status}（路由缺失）"
        );
    }

    let post_cases: &[(&str, &str)] = &[
        ("/subagent/run", r#"{"prompt":"hi","read_only":true}"#),
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
        ("/project/rules", r#"{"content":"project-rules"}"#),
    ];
    for (path, body) in post_cases {
        let status = client
            .post(format!("{base}{path}"))
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .unwrap()
            .status()
            .as_u16();
        assert!(
            status != 404 && status != 405,
            "POST {path} → {status}（路由缺失）"
        );
    }

    handle.abort();
}
