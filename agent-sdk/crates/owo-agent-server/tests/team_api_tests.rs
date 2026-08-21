//! team_api 契约测试（Agent 2 子任务 2）：导出/导入/脱敏评审/版本历史/审计。
//! 模块经 #[path] 独立编译。

#[path = "../src/team_api.rs"]
mod team_api;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use base64::Engine;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

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
    team_api::router(state)
}

/// 构造技能包（写入 pipeline.store 供导出）。
fn seed_package(state: &owo_agent_server::AppState, id: &str, version: &str, skill_md: &str) {
    use owo_agent_core::learn::ActionGraph;
    use owo_agent_core::learn::{FlowSkillManifest, FlowSkillPackage, Sensitivity};

    let pipeline = state.pipeline.lock().unwrap();
    let mut graph = ActionGraph::default();
    graph.add_node(
        "step1",
        owo_agent_core::learn::ActionType::Click,
        owo_agent_core::learn::SemanticAnchor {
            app_id: None,
            role: None,
            name: "发送按钮".to_string(),
            element_id: None,
            parent: None,
        },
        None,
        None,
    );
    let package = FlowSkillPackage {
        manifest: FlowSkillManifest {
            id: id.to_string(),
            name: id.to_string(),
            version: version.to_string(),
            min_app_version: "0.5.0".to_string(),
            target_apps: vec!["qq".to_string()],
            permissions: vec!["ui:operate".to_string()],
            variables: Vec::new(),
            sensitivity: Sensitivity::Medium,
        },
        graph,
        skill_md: format!("---\nname: {id}\n---\n\n{skill_md}"),
    };
    pipeline.store.save(&package).unwrap();
}

fn package_b64(package: &owo_agent_core::learn::FlowSkillPackage) -> String {
    let bytes = owo_agent_core::share_skill::export_flow_skill_package(package).unwrap();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn make_package(
    id: &str,
    version: &str,
    skill_md: &str,
) -> owo_agent_core::learn::FlowSkillPackage {
    use owo_agent_core::learn::{
        ActionGraph, FlowSkillManifest, FlowSkillPackage, SemanticAnchor, Sensitivity,
    };
    let mut graph = ActionGraph::default();
    graph.add_node(
        "step1",
        owo_agent_core::learn::ActionType::Type,
        SemanticAnchor {
            app_id: None,
            role: None,
            name: "输入区".to_string(),
            element_id: None,
            parent: None,
        },
        Some("hello".to_string()),
        None,
    );
    FlowSkillPackage {
        manifest: FlowSkillManifest {
            id: id.to_string(),
            name: id.to_string(),
            version: version.to_string(),
            min_app_version: "0.5.0".to_string(),
            target_apps: vec!["notepad".to_string()],
            permissions: vec!["ui:operate".to_string()],
            variables: Vec::new(),
            sensitivity: Sensitivity::Low,
        },
        graph,
        skill_md: format!("---\nname: {id}\n---\n\n{skill_md}"),
    }
}

// ---------- 导出 ----------

#[tokio::test]
async fn export_roundtrip_preserves_package() {
    let (state, _temp) = test_state();
    seed_package(&state, "demo-flow", "1.2.0", "# 演示技能\n\n按 F2 发送。");
    let (status, resp) = call(
        &router(state.clone()),
        request(
            "POST",
            "/team/export",
            Some(r#"{"type":"flow","id":"demo-flow"}"#),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "导出应成功：{resp}");
    assert_eq!(resp["manifest"]["id"], "demo-flow");
    assert_eq!(resp["manifest"]["version"], "1.2.0");
    assert!(resp["package_b64"].as_str().unwrap().len() > 10);
}

#[tokio::test]
async fn export_unknown_package_404_and_bad_type_400() {
    let (state, _temp) = test_state();
    let (status, _) = call(
        &router(state.clone()),
        request(
            "POST",
            "/team/export",
            Some(r#"{"type":"flow","id":"nope"}"#),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "未知包应 404");
    let (status, _) = call(
        &router(state.clone()),
        request("POST", "/team/export", Some(r#"{"type":"skill","id":"x"}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "不支持的类型应 400");
}

// ---------- 评审 / 导入 ----------

#[tokio::test]
async fn review_blocks_package_with_credentials() {
    let (state, _temp) = test_state();
    let package = make_package(
        "leaky",
        "1.0.0",
        "# 技能\n\n调用 OPENAI_API_KEY=sk-abc12345def67890ghi 获取结果",
    );
    let body = json!({ "package_b64": package_b64(&package) }).to_string();
    let (status, resp) = call(
        &router(state.clone()),
        request("POST", "/team/review", Some(&body)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "review 应 200：{resp}");
    assert_eq!(resp["blocked"], true, "含凭据应 blocked：{resp}");
    let findings = resp["findings"].as_array().unwrap();
    assert!(
        findings.iter().any(|f| f["category"] == "credential"),
        "应有凭据 finding：{findings:?}"
    );
}

#[tokio::test]
async fn review_blocks_dangerous_and_personal() {
    let (state, _temp) = test_state();
    let package = make_package(
        "risky",
        "1.0.0",
        "# 技能\n\nimport os\nos.system('rm -rf /')\n联系人 13800138000",
    );
    let body = json!({ "package_b64": package_b64(&package) }).to_string();
    let (status, resp) = call(
        &router(state.clone()),
        request("POST", "/team/review", Some(&body)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "review 应 200：{resp}");
    assert_eq!(resp["blocked"], true);
    let findings = resp["findings"].as_array().unwrap();
    assert!(findings.iter().any(|f| f["category"] == "dangerous"));
    assert!(findings.iter().any(|f| f["category"] == "personal"));
}

#[tokio::test]
async fn review_passes_clean_package() {
    let (state, _temp) = test_state();
    let package = make_package("clean", "1.0.0", "# 技能\n\n点击发送按钮完成发送。");
    let body = json!({ "package_b64": package_b64(&package) }).to_string();
    let (status, resp) = call(
        &router(state.clone()),
        request("POST", "/team/review", Some(&body)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "review 应 200：{resp}");
    assert_eq!(resp["blocked"], false, "干净包应通过：{resp}");
}

#[tokio::test]
async fn import_blocked_package_does_not_persist() {
    let (state, temp) = test_state();
    let package = make_package("blocked-pkg", "1.0.0", "# 技能\n\npassword: hunter2secret");
    let body = json!({ "package_b64": package_b64(&package) }).to_string();
    let (status, resp) = call(
        &router(state.clone()),
        request("POST", "/team/import", Some(&body)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "blocked 是 200 语义：{resp}");
    assert_eq!(resp["blocked"], true);
    assert!(!resp["findings"].as_array().unwrap().is_empty());
    // 未落盘：团队库无该包。
    assert!(
        !temp
            .path()
            .join("team")
            .join("skills")
            .join("blocked-pkg")
            .exists(),
        "blocked 包不应落盘"
    );
    // 版本历史为空。
    let (_, hist) = call(
        &router(state.clone()),
        request("GET", "/team/versions?id=blocked-pkg", None),
    )
    .await;
    assert_eq!(hist["count"], 0);
}

#[tokio::test]
async fn import_clean_package_persists_and_tracks_versions() {
    let (state, temp) = test_state();
    let package = make_package("shared-flow", "1.0.0", "# 技能\n\n发送消息并确认。");
    let body = json!({ "package_b64": package_b64(&package) }).to_string();
    let (status, resp) = call(
        &router(state.clone()),
        request("POST", "/team/import", Some(&body)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "导入应成功：{resp}");
    assert_eq!(resp["blocked"], false);
    // 落盘 + 版本历史 1 条。
    assert!(temp
        .path()
        .join("team")
        .join("skills")
        .join("shared-flow")
        .exists());
    let (_, hist) = call(
        &router(state.clone()),
        request("GET", "/team/versions?id=shared-flow", None),
    )
    .await;
    assert_eq!(hist["count"], 1);
    assert_eq!(hist["versions"][0]["version"], "1.0.0");

    // 再导入 v1.1.0 → 历史追加。
    let package_v2 = make_package("shared-flow", "1.1.0", "# 技能\n\n发送消息并确认。");
    let body = json!({ "package_b64": package_b64(&package_v2) }).to_string();
    let (status, _) = call(
        &router(state.clone()),
        request("POST", "/team/import", Some(&body)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, hist) = call(
        &router(state.clone()),
        request("GET", "/team/versions?id=shared-flow", None),
    )
    .await;
    assert_eq!(hist["count"], 2, "版本应追加：{hist}");
}

#[tokio::test]
async fn import_corrupt_package_400() {
    let (state, _temp) = test_state();
    let body = json!({ "package_b64": "bm90LWEtdmFsaWQtcGFja2FnZQ==" }).to_string();
    let (status, resp) = call(
        &router(state.clone()),
        request("POST", "/team/import", Some(&body)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "损坏包应 400：{resp}");
}

#[tokio::test]
async fn import_invalid_base64_400() {
    let (state, _temp) = test_state();
    let (status, _) = call(
        &router(state.clone()),
        request(
            "POST",
            "/team/import",
            Some(r#"{"package_b64":"!!not-base64!!"}"#),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ---------- 审计 ----------

#[tokio::test]
async fn audit_records_export_and_import() {
    let (state, _temp) = test_state();
    seed_package(&state, "audit-flow", "1.0.0", "# 技能");
    let (status, _) = call(
        &router(state.clone()),
        request(
            "POST",
            "/team/export",
            Some(r#"{"type":"flow","id":"audit-flow"}"#),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let package = make_package("audit-import", "1.0.0", "# 技能\n\n安全内容。");
    let body = json!({ "package_b64": package_b64(&package) }).to_string();
    let (status, _) = call(
        &router(state.clone()),
        request("POST", "/team/import", Some(&body)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, resp) = call(&router(state.clone()), request("GET", "/team/audit", None)).await;
    assert_eq!(status, StatusCode::OK);
    let entries = resp["entries"].as_array().unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e.as_str().unwrap_or("").contains("team/export")),
        "审计应含 export：{entries:?}"
    );
    assert!(
        entries
            .iter()
            .any(|e| e.as_str().unwrap_or("").contains("team/import")),
        "审计应含 import：{entries:?}"
    );
}
