//! 笔记 HTTP API 契约测试（Lane A）。
//!
//! 独立编译：`#[path = "../src/notes_api.rs"] mod notes_api;`。
//! 临时目录只用 tempfile；不触碰真实 data_root/workspace。

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use owo_agent_core::permissions::Policy;
use owo_agent_core::sqlite_store::SqliteSessionStore;
use owo_agent_core::tools::ToolRegistry;
use owo_agent_core::Agent;
use std::sync::Arc;
use tower::ServiceExt;

#[path = "../src/notes_api.rs"]
mod notes_api;

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

fn router(state: Arc<owo_agent_server::AppState>) -> axum::Router {
    notes_api::router(state)
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
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> (u16, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(request(method, path, body))
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, value)
}

async fn create(app: &axum::Router, title: &str, markdown: Option<&str>) -> (u16, String) {
    let body = match markdown {
        Some(md) => format!(
            r#"{{"title":"{title}","markdown":{}}}"#,
            serde_json::to_string(md).unwrap()
        ),
        None => format!(r#"{{"title":"{title}"}}"#),
    };
    let (status, value) = send(app, "POST", "/notes", Some(&body)).await;
    (status, value["id"].as_str().unwrap_or("").to_string())
}

// ---------------- 创建/列表/读取/删除 ----------------

#[tokio::test]
async fn create_list_get_delete_roundtrip() {
    let (state, _temp) = test_state().await;
    let app = router(state.clone());

    let (status, id) = create(
        &app,
        "第一份笔记",
        Some("# 标题\n\n正文段落。\n\n- 条目一\n- 条目二\n"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED.as_u16());
    assert!(!id.is_empty());

    let (status, list) = send(&app, "GET", "/notes", None).await;
    assert_eq!(status, 200);
    assert_eq!(list["count"], 1);
    assert_eq!(list["notes"][0]["title"], "第一份笔记");

    let (status, doc) = send(&app, "GET", &format!("/notes/{id}"), None).await;
    assert_eq!(status, 200);
    assert_eq!(doc["title"], "第一份笔记");
    assert!(doc["blocks"].is_object(), "应返回完整块树");

    let (status, _) = send(&app, "DELETE", &format!("/notes/{id}"), None).await;
    assert_eq!(status, 200);
    let (status, _) = send(&app, "GET", &format!("/notes/{id}"), None).await;
    assert_eq!(status, 404, "删除后读取应 404");
    let (_, list) = send(&app, "GET", "/notes", None).await;
    assert_eq!(list["count"], 0);
}

#[tokio::test]
async fn create_requires_title() {
    let (state, _temp) = test_state().await;
    let app = router(state);
    let (status, value) = send(&app, "POST", "/notes", Some(r#"{"title":""}"#)).await;
    assert_eq!(status, 400);
    assert!(value["error"].is_string());
}

#[tokio::test]
async fn get_unknown_note_is_404() {
    let (state, _temp) = test_state().await;
    let app = router(state);
    let (status, _) = send(&app, "GET", "/notes/no-such-id", None).await;
    assert_eq!(status, 404);
}

// ---------------- 整文档替换（PUT）与校验 ----------------

#[tokio::test]
async fn put_replaces_title_and_blocks() {
    let (state, _temp) = test_state().await;
    let app = router(state.clone());
    let (_, id) = create(&app, "旧标题", None).await;

    let (_, doc) = send(&app, "GET", &format!("/notes/{id}"), None).await;
    let blocks = doc["blocks"].clone();
    let body = format!(r#"{{"title":"新标题","blocks":{blocks}}}"#);
    let (status, value) = send(&app, "PUT", &format!("/notes/{id}"), Some(&body)).await;
    assert_eq!(status, 200, "{value}");
    let (_, doc) = send(&app, "GET", &format!("/notes/{id}"), None).await;
    assert_eq!(doc["title"], "新标题");
}

#[tokio::test]
async fn put_rejects_orphan_blocks() {
    let (state, _temp) = test_state().await;
    let app = router(state.clone());
    let (_, id) = create(&app, "t", None).await;
    // 构造孤儿块：blocks 含一个不被 root 引用的块
    let body = r#"{"title":"x","blocks":{"root":{"id":"root","kind":"Paragraph","text":"","attrs":{},"children":[]},"orphan":{"id":"orphan","kind":{"Paragraph":{"text":"孤儿"}},"attrs":{},"children":[]}}}"#;
    let (status, value) = send(&app, "PUT", &format!("/notes/{id}"), Some(body)).await;
    assert_eq!(status, 400, "孤儿块应被拒绝：{value}");
}

// ---------------- 块操作 ----------------

#[tokio::test]
async fn block_add_patch_delete_and_hierarchy() {
    let (state, _temp) = test_state().await;
    let app = router(state.clone());
    let (_, id) = create(&app, "块操作", None).await;

    // 添加段落块
    let (status, value) = send(
        &app,
        "POST",
        &format!("/notes/{id}/blocks"),
        Some(r#"{"kind":"paragraph","text":"新增段落"}"#),
    )
    .await;
    assert_eq!(status, 200, "{value}");
    let block_id = value["id"].as_str().unwrap().to_string();

    // 添加列表 + 列表项（after 语义）
    let (status, list_id) = {
        let (s, v) = send(
            &app,
            "POST",
            &format!("/notes/{id}/blocks"),
            Some(r#"{"kind":"list","data":{"ordered":false}}"#),
        )
        .await;
        (s, v["id"].as_str().unwrap().to_string())
    };
    assert_eq!(status, 200);
    let (status, _) = send(
        &app,
        "POST",
        &format!("/notes/{id}/blocks"),
        Some(&format!(
            r#"{{"parent":"{list_id}","kind":"list_item","text":"项一"}}"#
        )),
    )
    .await;
    assert_eq!(status, 200);

    // 移动段落块到列表下
    let (status, _) = send(
        &app,
        "POST",
        &format!("/notes/{id}/blocks/move"),
        Some(&format!(
            r#"{{"block_id":"{block_id}","parent":"{list_id}"}}"#
        )),
    )
    .await;
    assert_eq!(status, 200);

    // 验证层级
    let (_, doc) = send(&app, "GET", &format!("/notes/{id}"), None).await;
    assert!(doc["blocks"][&list_id]["children"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!(block_id)));

    // PATCH 文本
    let (status, _) = send(
        &app,
        "PATCH",
        &format!("/notes/{id}/blocks/{block_id}"),
        Some(r#"{"text":"更新后的文本"}"#),
    )
    .await;
    assert_eq!(status, 200);
    let (_, doc) = send(&app, "GET", &format!("/notes/{id}"), None).await;
    assert_eq!(
        doc["blocks"][&block_id]["kind"]["Paragraph"]["text"],
        "更新后的文本"
    );

    // 删除块
    let (status, value) = send(
        &app,
        "DELETE",
        &format!("/notes/{id}/blocks/{block_id}"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert!(value["removed"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!(block_id)));
    let (status, _) = send(
        &app,
        "PATCH",
        &format!("/notes/{id}/blocks/{block_id}"),
        Some(r#"{"text":"x"}"#),
    )
    .await;
    assert_eq!(status, 404, "已删除块应 404");
}

#[tokio::test]
async fn block_ops_error_paths() {
    let (state, _temp) = test_state().await;
    let app = router(state.clone());
    let (_, id) = create(&app, "错误路径", None).await;

    // 未知 kind
    let (status, _) = send(
        &app,
        "POST",
        &format!("/notes/{id}/blocks"),
        Some(r#"{"kind":"nonsense"}"#),
    )
    .await;
    assert_eq!(status, 400);
    // 未知父块
    let (status, _) = send(
        &app,
        "POST",
        &format!("/notes/{id}/blocks"),
        Some(r#"{"kind":"paragraph","parent":"nope","text":"x"}"#),
    )
    .await;
    assert_eq!(status, 400);
    // after 不存在
    let (status, _) = send(
        &app,
        "POST",
        &format!("/notes/{id}/blocks"),
        Some(r#"{"kind":"paragraph","after":"nope","text":"x"}"#),
    )
    .await;
    assert_eq!(status, 400);
    // 移动不存在的块
    let (status, _) = send(
        &app,
        "POST",
        &format!("/notes/{id}/blocks/move"),
        Some(r#"{"block_id":"nope"}"#),
    )
    .await;
    assert_eq!(status, 400);
    // 未知笔记上的块操作
    let (status, _) = send(
        &app,
        "POST",
        "/notes/zzz/blocks",
        Some(r#"{"kind":"paragraph","text":"x"}"#),
    )
    .await;
    assert_eq!(status, 404);
}

// ---------------- 导入与 MD 往返 ----------------

#[tokio::test]
async fn import_markdown_roundtrip_zero_block_loss() {
    let (state, _temp) = test_state().await;
    let app = router(state.clone());
    let md = "# 大标题\n\n## 小节\n\n正文。\n\n- 一\n- 二\n\n1. 甲\n2. 乙\n\n```rust\nfn main() {}\n```\n\n> 引用\n\n| A | B |\n| --- | --- |\n| 1 | 2 |\n\n![图](img.png)\n";
    let body = serde_json::json!({ "title": "导入", "markdown": md }).to_string();
    let (status, value) = send(&app, "POST", "/notes/import", Some(&body)).await;
    assert_eq!(status, 200, "{value}");
    let id = value["id"].as_str().unwrap().to_string();

    // md 导出 → 再导入 → 块 kind 序列一致（零丢块）
    let (_, exported) = send(&app, "GET", &format!("/notes/{id}/export/md"), None).await;
    let exported_md = exported["content"].as_str().unwrap();
    let body2 = serde_json::json!({ "title": "再导入", "markdown": exported_md }).to_string();
    let (_, value2) = send(&app, "POST", "/notes/import", Some(&body2)).await;
    let id2 = value2["id"].as_str().unwrap().to_string();
    let (_, doc1) = send(&app, "GET", &format!("/notes/{id}"), None).await;
    let (_, doc2) = send(&app, "GET", &format!("/notes/{id2}"), None).await;
    let kinds = |doc: &serde_json::Value| -> Vec<String> {
        doc["blocks"]
            .as_object()
            .unwrap()
            .values()
            .map(|b| serde_json::to_string(&b["kind"]).unwrap())
            .collect::<Vec<_>>()
    };
    let mut k1 = kinds(&doc1);
    let mut k2 = kinds(&doc2);
    k1.sort();
    k2.sort();
    assert_eq!(k1, k2, "MD 往返零丢块");
}

#[tokio::test]
async fn export_md_and_html() {
    let (state, _temp) = test_state().await;
    let app = router(state.clone());
    let (_, id) = create(&app, "导出", Some("# 标题\n\n文本。\n")).await;
    let (status, value) = send(&app, "GET", &format!("/notes/{id}/export/md"), None).await;
    assert_eq!(status, 200);
    assert!(value["content"].as_str().unwrap().contains("# 标题"));
    let (status, value) = send(&app, "GET", &format!("/notes/{id}/export/html"), None).await;
    assert_eq!(status, 200);
    assert!(value["content"].as_str().unwrap().contains("<h1>"));
    let (status, _) = send(&app, "GET", &format!("/notes/{id}/export/pdf"), None).await;
    assert_eq!(status, 400, "未知格式 400");
}

#[tokio::test]
async fn html_export_never_contains_script() {
    let (state, _temp) = test_state().await;
    let app = router(state.clone());
    let (_, id) = create(&app, "安全", None).await;
    // 注入恶意 HTML 块（text 走 sanitize）
    let (status, _) = send(
        &app,
        "POST",
        &format!("/notes/{id}/blocks"),
        Some(r#"{"kind":"html","text":"<p>好</p><script>alert(1)</script><img src=\"x\" onerror=\"e()\">"}"#),
    )
    .await;
    assert_eq!(status, 200);
    let (_, value) = send(&app, "GET", &format!("/notes/{id}/export/html"), None).await;
    let html = value["content"].as_str().unwrap();
    assert!(!html.contains("<script"), "HTML 导出不得含脚本");
    assert!(!html.contains("onerror"), "事件属性不得出现");
}

// ---------------- 搜索与重索引 ----------------

#[tokio::test]
async fn search_hits_and_miss() {
    let (state, _temp) = test_state().await;
    let app = router(state.clone());
    let (_, id) = create(&app, "检索", Some("# 量子计算\n\n关于量子纠缠的说明。\n")).await;

    let (status, value) = send(&app, "GET", "/notes/search?q=量子", None).await;
    assert_eq!(status, 200);
    assert!(value["count"].as_u64().unwrap() > 0, "应命中量子");
    let (status, value) = send(&app, "GET", "/notes/search?q=不存在的词xyzabc", None).await;
    assert_eq!(status, 200);
    let hit_other = value["hits"]
        .as_array()
        .unwrap()
        .iter()
        .any(|h| h["doc_id"].as_str() == Some(id.as_str()));
    assert!(!hit_other, "无关词不应命中该文档");
    // 缺 q → 400
    let (status, _) = send(&app, "GET", "/notes/search", None).await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn search_updates_after_block_change_and_reindex() {
    let (state, _temp) = test_state().await;
    let app = router(state.clone());
    let (_, id) = create(&app, "更新检索", None).await;

    // 添加含特征词的块 → 可搜到
    let (_, value) = send(
        &app,
        "POST",
        &format!("/notes/{id}/blocks"),
        Some(r#"{"kind":"paragraph","text":"特征词 zebra-alpha"}"#),
    )
    .await;
    let block_id = value["id"].as_str().unwrap().to_string();
    let (_, value) = send(&app, "GET", "/notes/search?q=zebra-alpha", None).await;
    assert!(value["count"].as_u64().unwrap() > 0);

    // 删除该块 → 不再命中
    send(
        &app,
        "DELETE",
        &format!("/notes/{id}/blocks/{block_id}"),
        None,
    )
    .await;
    let (_, value) = send(&app, "GET", "/notes/search?q=zebra-alpha", None).await;
    let hit = value["hits"]
        .as_array()
        .unwrap()
        .iter()
        .any(|h| h["doc_id"].as_str() == Some(id.as_str()));
    assert!(!hit, "删除块后不应再命中");

    // reindex 端点
    let (status, _) = send(&app, "POST", &format!("/notes/{id}/reindex"), None).await;
    assert_eq!(status, 200);
    let (status, _) = send(&app, "POST", "/notes/nope/reindex", None).await;
    assert_eq!(status, 404);
}

// ---------------- 审计 ----------------

#[tokio::test]
async fn writes_are_audited() {
    let (state, _temp) = test_state().await;
    let app = router(state.clone());
    let (_, id) = create(&app, "审计", None).await;
    send(
        &app,
        "POST",
        &format!("/notes/{id}/blocks"),
        Some(r#"{"kind":"paragraph","text":"x"}"#),
    )
    .await;
    send(&app, "DELETE", &format!("/notes/{id}"), None).await;
    let audit = state.agent.audit_log();
    let entries = audit.lock().unwrap();
    let events: Vec<&str> = entries.entries.iter().map(|e| e.event.as_str()).collect();
    assert!(events.contains(&"notes.create"));
    assert!(events.contains(&"notes.block.add"));
    assert!(events.contains(&"notes.delete"));
}
