//! plugin_market_api 契约测试（Lane B）：模块经 `#[path]` 独立编译。
//!
//! 覆盖：目录/seed/versions、verify/install/update/uninstall、
//! 签名通过/缺失拒绝/关闭后可装、高危扫描拒绝、更新失败保留旧版、
//! 审计尾部、400/404 错误路径。
//!
//! 签名用例复用"固定签名常量"（由 scripts/plugin-sign.py 对
//! (id=signed-ok, entry=server.py, 内容=print('signed-ok')) 生成，
//! 与 core::plugin::plugin_digest 摘要口径一致），避免引入额外依赖。

#[path = "../src/plugin_market_api.rs"]
mod plugin_market_api;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

// ---------- 固定签名常量（Ed25519，plugin-sign.py 生成） ----------

const SIGNED_MANIFEST: &str = r#"{
  "id": "signed-ok",
  "name": "Signed OK",
  "version": "1.0.0",
  "entry": "server.py",
  "signature": {
    "algorithm": "ed25519",
    "public_key_b64": "AP8Qj4eqSt/Oltnc4Og7HV6gOiLCRzaX8uMp2ZiTf1g=",
    "signature_b64": "rHMiPytHlZQ77gPR+mQZj7tiWStISx48hG4w8M9ObbE8KJudqqqGltHUvuXRjzPlFFEC4SfCVb+/1Rv1QXDwCw=="
  }
}"#;
const SIGNED_ENTRY: &str = "print('signed-ok')";

/// URL query 编码（仅处理反斜杠/空格/冒号等不安全字符；测试用）。
fn urlenc(s: &str) -> String {
    s.replace('\\', "%5C")
        .replace(' ', "%20")
        .replace(':', "%3A")
}

fn request(method: &str, path: &str, body: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(path);
    if let Some(b) = body {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
        builder.body(Body::from(b.to_string())).unwrap()
    } else {
        builder.body(Body::empty()).unwrap()
    }
}

async fn call(router: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let response = router.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

/// 最小 AppState（复用 route_contract_tests 的 IdleProvider 模式）。
fn test_state() -> (Arc<owo_agent_server::AppState>, tempfile::TempDir) {
    use owo_agent_core::agent::Agent;
    use owo_agent_core::gateway::ModelProvider;
    use owo_agent_core::permissions::Policy;
    use owo_agent_core::sqlite_store::SqliteSessionStore;
    use owo_agent_core::tools::ToolRegistry;

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

fn router(state: Arc<owo_agent_server::AppState>) -> Router {
    plugin_market_api::router(state)
}

/// 无签名插件目录（id/name/version/entry 可定制，entry 内容可定制）。
fn write_plugin(
    root: &std::path::Path,
    id: &str,
    version: &str,
    entry_content: &str,
) -> std::path::PathBuf {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let entry = if entry_content.is_empty() {
        None
    } else {
        Some("server.py")
    };
    let manifest = json!({
        "id": id,
        "name": id,
        "version": version,
        "entry": entry,
    });
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    if !entry_content.is_empty() {
        std::fs::write(dir.join("server.py"), entry_content).unwrap();
    }
    dir
}

/// 有效签名插件（固定常量：id=signed-ok，内容不可改）。
fn write_signed_plugin(root: &std::path::Path) -> std::path::PathBuf {
    let dir = root.join("signed-ok");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("manifest.json"), SIGNED_MANIFEST).unwrap();
    std::fs::write(dir.join("server.py"), SIGNED_ENTRY).unwrap();
    dir
}

// ---------- 目录 / seed / versions ----------

#[tokio::test]
async fn catalog_lists_local_plugins_with_app_version() {
    let (state, temp) = test_state();
    let _ = write_plugin(
        &temp.path().join("plugins"),
        "local-a",
        "1.0.0",
        "print('a')",
    );
    let (status, body) = call(&router(state), request("GET", "/plugins/market", None)).await;
    assert_eq!(status, StatusCode::OK);
    let plugins = body["plugins"].as_array().expect("plugins 数组");
    assert!(
        plugins.iter().any(|p| p["id"] == "local-a"),
        "应含本地插件：{plugins:?}"
    );
    assert_eq!(
        body["app_version"].as_str().unwrap(),
        env!("CARGO_PKG_VERSION")
    );
}

#[tokio::test]
async fn seed_writes_market_and_catalog_marks_market_source() {
    let (state, temp) = test_state();
    let seed_body = r#"{"entries":[{"id":"owo.plugin.demo","name":"Demo","version":"2.0.0","min_app_version":"0.5.0"}]}"#;
    let (status, body) = call(
        &router(state.clone()),
        request("POST", "/plugins/market/seed", Some(seed_body)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["entries"], 1);
    assert!(
        temp.path().join("plugins").join("market.json").exists(),
        "market.json 应落盘"
    );

    let (_, catalog) = call(
        &router(state.clone()),
        request("GET", "/plugins/market", None),
    )
    .await;
    let demo = catalog["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "owo.plugin.demo")
        .unwrap();
    assert_eq!(demo["source"], "market");
    assert_eq!(demo["version"], "2.0.0");
}

#[tokio::test]
async fn versions_resolves_compatible_and_404_for_missing() {
    let (state, temp) = test_state();
    let ws_plugins = temp.path().join("ws").join("plugins").join("vplug");
    std::fs::create_dir_all(&ws_plugins).unwrap();
    std::fs::write(
        ws_plugins.join("versions.json"),
        r#"{"compatibility":{"1.0.0":"0.5.0","1.1.0":"0.6.0"}}"#,
    )
    .unwrap();
    let (status, body) = call(
        &router(state.clone()),
        request("GET", "/plugins/market/versions?id=vplug&app=0.5.8", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["latest_compatible"], "1.0.0", "0.5.8 下 1.1.0 不兼容");

    let (status, _) = call(
        &router(state.clone()),
        request("GET", "/plugins/market/versions?id=nope&app=0.5.8", None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "无 versions.json 应 404");
}

// ---------- verify / install / update / uninstall（串行：env 与静态 manager 键控） ----------

/// 签名/安装语义依赖进程级 env（OWO_PLUGIN_REQUIRE_SIGNATURE）与静态 manager，
/// 并行会竞态，故合并为单个串行用例执行。
#[test]
fn signature_install_flow_serial() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        // 阶段 1：默认强制签名。
        std::env::set_var("OWO_PLUGIN_REQUIRE_SIGNATURE", "1");
        let (state, temp) = test_state();
        // 缺签名拒绝。
        let dir = write_plugin(temp.path(), "nosig", "1.0.0", "print('x')");
        let body = json!({ "dir": dir.display().to_string() }).to_string();
        let (status, resp) = call(
            &router(state.clone()),
            request("POST", "/plugins/market/install", Some(&body)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "缺签名应拒绝：{resp}");
        assert!(resp["error"].as_str().unwrap_or("").contains("签名"));
        // 有效签名插件 verify + install。
        let signed = write_signed_plugin(temp.path());
        let body = json!({ "dir": signed.display().to_string() }).to_string();
        let (status, resp) = call(
            &router(state.clone()),
            request("POST", "/plugins/market/verify", Some(&body)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "签名插件校验应通过：{resp}");
        let (status, resp) = call(
            &router(state.clone()),
            request("POST", "/plugins/market/install", Some(&body)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "签名插件安装应成功：{resp}");
        assert_eq!(resp["report"]["state"], "Activated");
        // 篡改拒绝。
        let dir2 = signed.clone();
        std::fs::write(dir2.join("server.py"), "print('EVIL')").unwrap();
        let body2 = json!({ "dir": dir2.display().to_string() }).to_string();
        let (status, resp) = call(
            &router(state.clone()),
            request("POST", "/plugins/market/install", Some(&body2)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "篡改应拒绝：{resp}");
        assert!(resp["error"].as_str().unwrap_or("").contains("签名"));
        // 高危（签名关闭后仍被扫描拦）。
        std::env::set_var("OWO_PLUGIN_REQUIRE_SIGNATURE", "0");
        let evil = write_plugin(
            temp.path(),
            "evil",
            "1.0.0",
            "import os; os.system('rm -rf /')",
        );
        let body = json!({ "dir": evil.display().to_string() }).to_string();
        let (status, resp) = call(
            &router(state.clone()),
            request("POST", "/plugins/market/install", Some(&body)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "高危应拒绝：{resp}");
        assert!(resp["error"].as_str().unwrap_or("").contains("静态扫描"));
        // 签名关闭后无签名插件可装。
        let lax = write_plugin(temp.path(), "lax", "1.0.0", "print('lax')");
        let body = json!({ "dir": lax.display().to_string() }).to_string();
        let (status, resp) = call(
            &router(state.clone()),
            request("POST", "/plugins/market/install", Some(&body)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "签名关闭后应可安装：{resp}");
        // 已安装插件 update 成功（新版同 id）+ 高危新版拒绝且旧版保留。
        let old = write_plugin(&temp.path().join("old"), "up", "1.0.0", "print('v1')");
        let body = json!({ "dir": old.display().to_string() }).to_string();
        let (status, _) = call(
            &router(state.clone()),
            request("POST", "/plugins/market/install", Some(&body)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let new = write_plugin(&temp.path().join("new"), "up", "2.0.0", "print('v2')");
        let body = json!({ "id": "up", "dir": new.display().to_string() }).to_string();
        let (status, resp) = call(
            &router(state.clone()),
            request("POST", "/plugins/market/update", Some(&body)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "更新应成功：{resp}");
        assert_eq!(resp["report"]["version"], "2.0.0");
        let evil_new = write_plugin(
            &temp.path().join("evil2"),
            "up",
            "3.0.0",
            "subprocess.Popen(['rm'])",
        );
        let body = json!({ "id": "up", "dir": evil_new.display().to_string() }).to_string();
        let (status, resp) = call(
            &router(state.clone()),
            request("POST", "/plugins/market/update", Some(&body)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "高危更新应拒绝：{resp}");
        let activated =
            std::fs::read_to_string(temp.path().join("plugins").join("up").join("server.py"))
                .unwrap();
        assert!(activated.contains("v2"), "旧版应保留：{activated}");
        // 卸载返回被移除文件；未知 id 404。
        let (status, resp) = call(
            &router(state.clone()),
            request("POST", "/plugins/market/uninstall", Some(r#"{"id":"up"}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let removed = resp["removed"].as_array().unwrap();
        assert!(
            removed
                .iter()
                .any(|f| f.as_str().unwrap_or("").contains("manifest.json")),
            "应返回被移除文件：{removed:?}"
        );
        let (status, _) = call(
            &router(state.clone()),
            request(
                "POST",
                "/plugins/market/uninstall",
                Some(r#"{"id":"nope"}"#),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "未知插件应 404");
        // 审计含写操作。
        let (status, resp) = call(
            &router(state.clone()),
            request("GET", "/plugins/market/audit?n=20", None),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let entries = resp["entries"].as_array().unwrap();
        assert!(
            entries
                .iter()
                .any(|e| e.as_str().unwrap_or("").contains("market/install")),
            "审计应含 install：{entries:?}"
        );
    });
}

// ---------- scan / audit / 错误路径 ----------

#[tokio::test]
async fn scan_reports_risks_and_passes_clean() {
    let (state, temp) = test_state();
    let clean = write_plugin(temp.path(), "clean", "1.0.0", "def f(x): return x");
    let (status, resp) = call(
        &router(state.clone()),
        request(
            "GET",
            &format!(
                "/plugins/market/scan?dir={}",
                urlenc(&clean.display().to_string())
            ),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resp["pass"], true, "干净插件应通过：{resp}");

    let risky = write_plugin(
        temp.path(),
        "risky",
        "1.0.0",
        "import socket; socket.socket()",
    );
    let (status, resp) = call(
        &router(state.clone()),
        request(
            "GET",
            &format!(
                "/plugins/market/scan?dir={}",
                urlenc(&risky.display().to_string())
            ),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resp["pass"], false, "网络调用未白名单应不通过：{resp}");

    let (status, _) = call(
        &router(state.clone()),
        request("GET", "/plugins/market/scan?dir=C:\\nonexistent-xyz", None),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "目录不存在应 400");
}

#[tokio::test]
async fn audit_tail_records_write_operations() {
    let (state, _temp) = test_state();
    let _ = &_temp;
    let seed_body = r#"{"entries":[{"id":"audit-demo","name":"A","version":"1.0.0"}]}"#;
    let (status, _) = call(
        &router(state.clone()),
        request("POST", "/plugins/market/seed", Some(seed_body)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, resp) = call(
        &router(state.clone()),
        request("GET", "/plugins/market/audit?n=10", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entries = resp["entries"].as_array().unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e.as_str().unwrap_or("").contains("market/seed")),
        "审计应含 seed：{entries:?}"
    );
}

#[tokio::test]
async fn missing_body_returns_client_error() {
    let (state, _temp) = test_state();
    let _ = &_temp;
    let (status, _) = call(
        &router(state.clone()),
        request("POST", "/plugins/market/install", None),
    )
    .await;
    assert!(status.is_client_error(), "缺 body 应 4xx（实际 {status}）");
}

#[tokio::test]
async fn seed_rejects_empty_id() {
    let (state, _temp) = test_state();
    let _ = &_temp;
    let (status, resp) = call(
        &router(state.clone()),
        request(
            "POST",
            "/plugins/market/seed",
            Some(r#"{"entries":[{"id":""}]}"#),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "空 id 应 400：{resp}");
}

#[tokio::test]
async fn verify_missing_manifest_400() {
    let (state, temp) = test_state();
    let dir = temp.path().join("no-manifest");
    std::fs::create_dir_all(&dir).unwrap();
    let body = json!({ "dir": dir.display().to_string() }).to_string();
    let (status, resp) = call(
        &router(state.clone()),
        request("POST", "/plugins/market/verify", Some(&body)),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "缺 manifest 应 400：{resp}"
    );
}
