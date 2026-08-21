//! R8 存储运维契约测试（最小）：/storage/backup|restore|export|clear 主链路冒烟。
//! 复用 route_contract_tests 的构造模式：真实 HTTP 面（tower oneshot）+ bearer token。

use owo_agent_core::permissions::Policy;
use owo_agent_core::sqlite_store::SqliteSessionStore;
use owo_agent_core::tools::ToolRegistry;
use owo_agent_core::Agent;
use owo_agent_server::build_router;
use std::sync::Arc;
use tower::ServiceExt;

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
    // 与真实 serve 布局一致：index.db 位于 data_root（temp.path()）下。
    let store = SqliteSessionStore::open(&temp.path().join("index.db")).unwrap();
    let state = Arc::new(owo_agent_server::AppState::new(
        agent,
        store,
        workspace.join("traces"),
        temp.path().to_path_buf(),
        workspace,
    ));
    (state, temp)
}

fn request(
    state: &Arc<owo_agent_server::AppState>,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> axum::http::Request<axum::body::Body> {
    use axum::http::{header, Method, Request};
    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(path)
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", state.auth_token.token()),
        );
    if let Some(body) = body {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
        return builder
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
    }
    builder.body(axum::body::Body::empty()).unwrap()
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// 备份 → 恢复 主链路：backup 产出可解包 zip；restore 接受该 b64 并自动先备份。
#[tokio::test]
async fn backup_and_restore_round_trip() {
    let (state, _temp) = test_state().await;
    let app = build_router(Arc::clone(&state));

    let backup = app
        .clone()
        .oneshot(request(&state, "POST", "/storage/backup", Some("{}")))
        .await
        .unwrap();
    assert_eq!(backup.status().as_u16(), 200, "backup 应 200");
    let backup_json = body_json(backup).await;
    let archive_b64 = backup_json["archive_b64"]
        .as_str()
        .expect("备份应含 archive_b64");
    assert!(archive_b64.len() > 64, "备份不应为空");
    assert!(backup_json["saved_to"]
        .as_str()
        .unwrap()
        .contains("backup-"));

    // 解包验证：manifest.json 存在且列出条目。
    use std::io::Read;
    let bytes =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, archive_b64).unwrap();
    let cursor = std::io::Cursor::new(&bytes);
    let mut archive = zip::ZipArchive::new(cursor).unwrap();
    let mut manifest = String::new();
    archive
        .by_name("manifest.json")
        .unwrap()
        .read_to_string(&mut manifest)
        .unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    assert_eq!(manifest["kind"], "backup");
    assert!(
        manifest["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e == "index.db"),
        "备份清单应含 index.db"
    );

    // restore 主链路：接受备份 b64。
    let restore = app
        .clone()
        .oneshot(request(
            &state,
            "POST",
            "/storage/restore",
            Some(&format!(r#"{{"archive_b64":"{archive_b64}"}}"#)),
        ))
        .await
        .unwrap();
    assert_eq!(restore.status().as_u16(), 200, "restore 应 200");
    let restore_json = body_json(restore).await;
    assert_eq!(restore_json["ok"], true);
    assert!(
        restore_json["pre_backup"]
            .as_str()
            .unwrap()
            .contains("pre-restore-"),
        "恢复前必须自动备份"
    );
    assert!(
        restore_json["restored"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e == "settings.json")
            || restore_json["staged"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e == "index.db"),
        "恢复应含 settings 或 index.db"
    );

    // 缺 archive_b64 → 400（路由存在性 + 参数校验）。
    let bad = app
        .clone()
        .oneshot(request(&state, "POST", "/storage/restore", Some("{}")))
        .await
        .unwrap();
    assert_eq!(bad.status().as_u16(), 400);
}

/// 导出主链路：全量标准 JSON，含 counts 与 sessions。
#[tokio::test]
async fn export_contains_full_standard_sections() {
    let (state, _temp) = test_state().await;
    let app = build_router(Arc::clone(&state));

    // 建一个会话，使导出非空。
    let created = app
        .clone()
        .oneshot(request(
            &state,
            "POST",
            "/session",
            Some(r#"{"workspace":".","model":"idle"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(created.status().as_u16(), 200);

    let export = app
        .clone()
        .oneshot(request(&state, "POST", "/storage/export", Some("{}")))
        .await
        .unwrap();
    assert_eq!(export.status().as_u16(), 200);
    let export_json = body_json(export).await;
    assert_eq!(export_json["format_version"], 1);
    for section in [
        "sessions",
        "audit",
        "notes",
        "skills",
        "workflows",
        "settings",
    ] {
        assert!(export_json.get(section).is_some(), "导出应含 {section} 节");
    }
    assert!(
        !export_json["sessions"].as_array().unwrap().is_empty(),
        "已建会话应出现在导出中"
    );
}

/// 清空主链路：需二次确认；确认后会话/审计为空且完整性通过。
#[tokio::test]
async fn clear_requires_confirm_and_validates_integrity() {
    let (state, _temp) = test_state().await;
    let app = build_router(Arc::clone(&state));
    let created = app
        .clone()
        .oneshot(request(
            &state,
            "POST",
            "/session",
            Some(r#"{"workspace":".","model":"idle"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(created.status().as_u16(), 200);

    // 无确认 → 400。
    let no_confirm = app
        .clone()
        .oneshot(request(&state, "POST", "/storage/clear", Some("{}")))
        .await
        .unwrap();
    assert_eq!(no_confirm.status().as_u16(), 400, "缺少二次确认应 400");

    // 二次确认 → 200 + 完整性 ok。
    let cleared = app
        .clone()
        .oneshot(request(
            &state,
            "POST",
            "/storage/clear",
            Some(r#"{"confirm":"CLEAR_ALL"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(cleared.status().as_u16(), 200);
    let cleared_json = body_json(cleared).await;
    assert_eq!(cleared_json["integrity"], "ok", "清空后完整性校验必须通过");
    assert!(
        cleared_json["cleared"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e == "sessions"),
        "清空清单应含 sessions"
    );

    // 会话列表应为空。
    let sessions = app
        .clone()
        .oneshot(request(&state, "GET", "/sessions", None))
        .await
        .unwrap();
    let list = body_json(sessions).await;
    assert_eq!(list.as_array().unwrap().len(), 0, "清空后会话列表应为空");
}
