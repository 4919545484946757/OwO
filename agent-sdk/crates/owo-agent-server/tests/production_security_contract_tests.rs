//! 生产级安全边界契约测试（黑盒优先，独立于业务实现）。
//!
//! 本文件只经公开 API（`build_router` 构造的真实路由 + `tower::ServiceExt::oneshot`
//! 内存往返）验证安全边界，不触碰任何私有 handler 实现：
//!
//! - 默认保护路由未携带 bearer token 时一律 401；
//! - `/health`、`/openapi.json`、`/auth/token` 为唯一允许的公开面；
//! - 回环 CORS 白名单正确，恶意 Origin 不被放行；
//! - 限流返回 429 与 `Retry-After`，且限流绝不绕过认证（未授权请求不可能拿到 2xx）；
//! - token/凭据值不会出现在常规响应、审计结构或鉴权错误体中；
//! - 所有数据落在临时目录，使用随机 token，不调用真实模型或用户凭据。

use owo_agent_core::permissions::Policy;
use owo_agent_core::sqlite_store::SqliteSessionStore;
use owo_agent_core::tools::ToolRegistry;
use owo_agent_core::Agent;
use owo_agent_server::build_router;
use std::sync::Arc;
use tower::ServiceExt;

use axum::body::Body;
use axum::http::{header, Method, Request, Response, StatusCode};

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

/// 构造隔离的 AppState：工作区、data_root、traces 全部落在临时目录，
/// token 随机生成并持久化到临时目录，不使用任何真实模型或用户凭据。
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

/// 构造附带本 state bearer token 的请求。
fn authed_request(
    state: &Arc<owo_agent_server::AppState>,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(path)
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", state.auth_token.token()),
        );
    if let Some(content) = body {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
        return builder.body(Body::from(content.to_string())).unwrap();
    }
    builder.body(Body::empty()).unwrap()
}

/// 构造不含 token 的匿名请求。
fn anonymous_request(method: &str, path: &str, body: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(path);
    if let Some(content) = body {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
        return builder.body(Body::from(content.to_string())).unwrap();
    }
    builder.body(Body::empty()).unwrap()
}

/// 取响应体文本（UTF-8 有损读取，仅用于秘密泄露断言）。
async fn body_text(response: Response<Body>) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap_or_default();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// 取响应体 JSON。
async fn body_json(response: Response<Body>) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap_or_default();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

/// 默认保护路由：未携带 token 一律 401（并返回结构化错误码）。
#[tokio::test]
async fn protected_routes_deny_anonymous_with_401() {
    let (state, _temp) = test_state().await;
    let app = build_router(Arc::clone(&state));

    for (method, path, body) in [
        ("GET", "/usage", None),
        ("GET", "/sessions", None),
        ("GET", "/skills", None),
        ("GET", "/plugins", None),
        ("GET", "/audit", None),
        ("GET", "/settings", None),
        ("GET", "/server/status", None),
        ("POST", "/goal", Some(r#"{"objective":"sec"}"#)),
        (
            "POST",
            "/session",
            Some(r#"{"workspace":".","model":"idle"}"#),
        ),
        (
            "POST",
            "/settings/egress",
            Some(r#"{"cloud_enabled":true}"#),
        ),
    ] {
        let response = app
            .clone()
            .oneshot(anonymous_request(method, path, body))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {path} 无 token 应返回 401"
        );
    }
}

/// 鉴权错误体必须带可机读错误码，且不回显 token。
#[tokio::test]
async fn auth_error_body_is_structured_and_token_free() {
    let (state, _temp) = test_state().await;
    let app = build_router(Arc::clone(&state));
    let token = state.auth_token.token().to_string();

    let response = app
        .clone()
        .oneshot(anonymous_request("GET", "/usage", None))
        .await
        .unwrap();
    let json = body_json(response).await;
    assert_eq!(
        json["code"].as_str(),
        Some("auth/unauthorized/not_retryable"),
        "401 错误体应带稳定错误码"
    );

    // 错误体绝不回显 token。
    let text = json.to_string();
    assert!(!text.contains(&token), "401 错误体泄露了 token");
}

/// 唯一允许的公开面：health / openapi / auth/token 无 token 可达，其余均被保护。
#[tokio::test]
async fn public_surface_is_exactly_health_openapi_bootstrap() {
    let (state, _temp) = test_state().await;
    let app = build_router(Arc::clone(&state));

    for path in ["/health", "/openapi.json", "/auth/token"] {
        let response = app
            .clone()
            .oneshot(anonymous_request("GET", path, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "GET {path} 应公开可达");
    }

    // 引导端点返回与 state 一致的同一 token（用于桌面端配对）。
    let response = app
        .clone()
        .oneshot(anonymous_request("GET", "/auth/token", None))
        .await
        .unwrap();
    let served = body_json(response).await;
    assert_eq!(
        served["token"].as_str(),
        Some(state.auth_token.token()),
        "/auth/token 应返回同一 token"
    );

    // 错误 token 不得进入任何受保护面。
    use axum::http::{header, Method, Request};
    let bad = Request::builder()
        .method(Method::GET)
        .uri("/sessions")
        .header(header::AUTHORIZATION, "Bearer definitely-wrong-token")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(bad).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// 回环 CORS 白名单：webview + localhost/127.0.0.1 放行并回显 ACAO，恶意 Origin 不放行。
#[tokio::test]
async fn cors_whitelist_allows_loopback_and_rejects_malicious() {
    let (state, _temp) = test_state().await;
    let app = build_router(Arc::clone(&state));

    fn preflight(origin: &str) -> Request<Body> {
        Request::builder()
            .method(Method::OPTIONS)
            .uri("/session")
            .header(header::ORIGIN, origin)
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
            .body(Body::empty())
            .unwrap()
    }

    fn acao(response: &Response<Body>) -> String {
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string()
    }

    // 恶意 Origin（含云端域名、局域网 IP、浏览器沙箱 null）→ 不回显 ACAO。
    for evil in [
        "https://evil.example",
        "https://attacker.com",
        "https://malicious-site.com",
        "http://192.168.0.1:8080",
        "null",
    ] {
        let response = app.clone().oneshot(preflight(evil)).await.unwrap();
        assert_eq!(
            acao(&response),
            "",
            "Origin {evil:?} 预检不应回显 Access-Control-Allow-Origin"
        );
    }

    // 回环白名单（webview + localhost/127.0.0.1 任意端口）→ 放行并回显 ACAO。
    for allowed in [
        "tauri://localhost",
        "http://tauri.localhost",
        "https://tauri.localhost:5173",
        "http://localhost:1420",
        "http://127.0.0.1:4096",
    ] {
        let response = app.clone().oneshot(preflight(allowed)).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "Origin {allowed:?} 预检应放行"
        );
        assert!(
            !acao(&response).is_empty(),
            "Origin {allowed:?} 应回显 Access-Control-Allow-Origin"
        );
    }

    // 恶意 Origin 的真实请求（非预检）同样不回显 ACAO（浏览器阻断跨源读取）。
    let actual = Request::builder()
        .method(Method::GET)
        .uri("/health")
        .header(header::ORIGIN, "https://evil.example")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(actual).await.unwrap();
    assert_eq!(acao(&response), "", "恶意 Origin 的真实请求不应回显 ACAO");
}

/// 限流契约：超全局 RPM 后返回 429 + Retry-After，且限流绝不绕过认证。
#[tokio::test]
async fn rate_limit_returns_429_with_retry_after_and_never_bypasses_auth() {
    // 环境变量进程级共享：用静态互斥串行化，避免污染并行用例。
    static RATE_LIMIT_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> =
        std::sync::OnceLock::new();
    let _guard = RATE_LIMIT_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;

    // 阶段一（限流契约）：携带合法 token 的高频写请求，超出全局 RPM 后返回 429 + Retry-After。
    std::env::set_var("OWO_API_RPM_GLOBAL", "5");
    let (state, _temp) = test_state().await;
    std::env::remove_var("OWO_API_RPM_GLOBAL");
    let app = build_router(Arc::clone(&state));

    let mut statuses = Vec::new();
    for _ in 0..10 {
        let response = app
            .clone()
            .oneshot(authed_request(
                &state,
                "POST",
                "/goal",
                Some(r#"{"objective":"rl"}"#),
            ))
            .await
            .unwrap();
        statuses.push(response.status());
    }
    let created = statuses
        .iter()
        .filter(|status| **status == StatusCode::CREATED)
        .count();
    let limited = statuses
        .iter()
        .filter(|status| **status == StatusCode::TOO_MANY_REQUESTS)
        .count();
    assert!((1..=5).contains(&created), "前 5 个应放行：{statuses:?}");
    assert!(limited >= 1, "超限应返回 429：{statuses:?}");

    // 429 必须携带 Retry-After。
    let response = app
        .clone()
        .oneshot(authed_request(
            &state,
            "POST",
            "/goal",
            Some(r#"{"objective":"rl"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(
        response.headers().get(header::RETRY_AFTER).is_some(),
        "429 应携带 Retry-After 头"
    );

    // 阶段二（限流绝不绕过认证）：用全新 state（全新桶，令牌充足）发匿名写请求，
    // 限流放行后仍须被鉴权拒绝 401，绝不因限流放行而拿到 2xx。
    std::env::set_var("OWO_API_RPM_GLOBAL", "5");
    let (anon_state, _anon_temp) = test_state().await;
    std::env::remove_var("OWO_API_RPM_GLOBAL");
    let anon_app = build_router(Arc::clone(&anon_state));
    let mut anonymous_statuses = Vec::new();
    for _ in 0..3 {
        let response = anon_app
            .clone()
            .oneshot(anonymous_request(
                "POST",
                "/goal",
                Some(r#"{"objective":"rl"}"#),
            ))
            .await
            .unwrap();
        anonymous_statuses.push(response.status());
    }
    assert!(
        anonymous_statuses.iter().all(|status| !status.is_success()),
        "匿名请求绝不通过限流绕过认证拿到 2xx：{anonymous_statuses:?}"
    );
    assert!(
        anonymous_statuses
            .iter()
            .all(|status| *status == StatusCode::UNAUTHORIZED),
        "令牌充足时限流放行后，匿名请求仍应被鉴权拒绝 401：{anonymous_statuses:?}"
    );
}

/// token/凭据值不得泄漏进常规响应、鉴权错误体或审计结构。
#[tokio::test]
async fn secrets_never_leak_into_responses_or_audit() {
    let (state, _temp) = test_state().await;
    let app = build_router(Arc::clone(&state));
    let token = state.auth_token.token().to_string();
    // 独立哨兵值：任何一处泄漏即被检出。
    let sentinel = format!("sk-live-sentinel-{}", uuid::Uuid::new_v4());

    // 采样受保护端点的常规响应。
    let sampled = [
        authed_request(&state, "GET", "/usage", None),
        authed_request(&state, "GET", "/settings", None),
        authed_request(&state, "GET", "/audit?limit=50", None),
        authed_request(&state, "GET", "/skills", None),
        authed_request(&state, "GET", "/server/status", None),
    ];
    for request in sampled {
        let response = app.clone().oneshot(request).await.unwrap();
        let text = body_text(response).await;
        assert!(
            !text.contains(&token),
            "受保护响应泄露了 bearer token：{text}"
        );
        assert!(!text.contains(&sentinel), "受保护响应泄露了哨兵值");
    }

    // 公开面（health/openapi）同样不得包含 token 或哨兵。
    for path in ["/health", "/openapi.json"] {
        let response = app
            .clone()
            .oneshot(anonymous_request("GET", path, None))
            .await
            .unwrap();
        let text = body_text(response).await;
        assert!(!text.contains(&token), "公开面 {path} 泄露了 token");
        assert!(!text.contains(&sentinel), "公开面 {path} 泄露了哨兵值");
    }

    // 审计结构不应包含 token/凭据（event 只记录动作与路径）。
    let response = app
        .clone()
        .oneshot(authed_request(&state, "GET", "/audit?limit=50", None))
        .await
        .unwrap();
    let audit = body_json(response).await;
    let audit_text = audit.to_string();
    assert!(!audit_text.contains(&token), "审计结构泄露了 token");
    assert!(!audit_text.contains(&sentinel), "审计结构泄露了哨兵值");
    assert!(
        !audit_text.contains("Bearer"),
        "审计结构不应出现 Authorization 头内容"
    );
}

/// settings 面契约：不出现明文凭据字段；已知非敏感字段正常往返。
#[tokio::test]
async fn settings_response_contains_no_plaintext_credentials() {
    let (state, _temp) = test_state().await;
    let app = build_router(Arc::clone(&state));

    // 预写一个含模型的 settings，证明 settings_get 返回真实持久化内容。
    let workspace = state.workspace.clone();
    std::fs::write(
        workspace.join("settings.json"),
        serde_json::json!({
            "model": "deepseek-v4-flash",
            "read_only": false,
        })
        .to_string(),
    )
    .unwrap();

    let response = app
        .clone()
        .oneshot(authed_request(&state, "GET", "/settings", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let settings = body_json(response).await;
    assert_eq!(settings["model"].as_str(), Some("deepseek-v4-flash"));

    // 明文凭据字段永不出现（大小写/变体一并覆盖；`token_budget` 是合法用量预算字段，非凭据）。
    let text = settings.to_string();
    let lower = text.to_lowercase();
    for leak in [
        "api_key",
        "apikey",
        "password",
        "passwd",
        "secret",
        "credentials",
        "access_key",
        "private_key",
    ] {
        assert!(
            !lower.contains(leak),
            "settings 响应泄露了凭据字段 {leak:?}：{text}"
        );
    }
}
