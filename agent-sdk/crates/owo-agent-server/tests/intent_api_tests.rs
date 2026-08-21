//! 统一命令入口 API 契约测试（Lane：多模态入口 · 子任务 2）。
//!
//! 覆盖：五类意图解析与参数抽取、未知意图兜底、command/run 文本模式动作路由、
//! voice 缺 wav/STT 未就绪 → 400/503、审计记录存在。全部离线（无凭据不判失败）。

#[path = "../src/intent_api.rs"]
mod intent_api;

use base64::Engine as _;
use owo_agent_server::AppState;
use serde_json::{json, Value};
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

async fn test_state() -> (Arc<AppState>, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let agent = owo_agent_core::Agent::new(
        Arc::new(IdleProvider),
        owo_agent_core::tools::ToolRegistry::new(),
        owo_agent_core::permissions::Policy::new(&workspace),
        Default::default(),
    );
    let store = owo_agent_core::sqlite_store::SqliteSessionStore::open(&workspace.join("index.db"))
        .unwrap();
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
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn parse(app: &axum::Router, text: &str) -> (u16, Value) {
    let body = json!({ "text": text }).to_string();
    call(app, "POST", "/intent/parse", Some(&body)).await
}

#[tokio::test]
async fn parse_five_intents_and_params() {
    let (state, _temp) = test_state().await;
    let app = intent_api::router(Arc::clone(&state));

    let (_, wf) = parse(&app, "运行工作流：报告生成").await;
    assert_eq!(wf["intent"], "run_workflow");
    assert_eq!(wf["params"]["workflow"], "报告生成");
    assert!(wf["confidence"].as_f64().unwrap() > 0.9);

    let (_, goal) = parse(&app, "创建目标：整理桌面文件").await;
    assert_eq!(goal["intent"], "create_goal");
    assert!(goal["params"]["objective"]
        .as_str()
        .unwrap()
        .contains("整理桌面文件"));

    let (_, note) = parse(&app, "查笔记：会议纪要").await;
    assert_eq!(note["intent"], "query_note");
    assert!(note["params"]["query"]
        .as_str()
        .unwrap()
        .contains("会议纪要"));

    let (_, mem) = parse(&app, "搜索记忆：张子豪").await;
    assert_eq!(mem["intent"], "search_memory");

    let (_, desktop) = parse(&app, "点击：发送按钮").await;
    assert_eq!(desktop["intent"], "desktop_action");
    assert_eq!(desktop["params"]["target"], "发送按钮");
}

#[tokio::test]
async fn parse_unknown_intent_falls_back_to_ask_agent() {
    let (state, _temp) = test_state().await;
    let app = intent_api::router(Arc::clone(&state));
    let (_, parsed) = parse(&app, "今天天气怎么样").await;
    assert_eq!(parsed["intent"], "ask_agent");
    assert!(parsed["confidence"].as_f64().unwrap() < 0.5);
}

#[tokio::test]
async fn parse_empty_text_400() {
    let (state, _temp) = test_state().await;
    let app = intent_api::router(Arc::clone(&state));
    let (status, value) = call(&app, "POST", "/intent/parse", Some(r#"{"text":"  "}"#)).await;
    assert_eq!(status, 400);
    assert!(value["error"].as_str().unwrap().contains("text"));
}

#[tokio::test]
async fn command_run_create_goal_works_offline() {
    let (state, _temp) = test_state().await;
    let app = intent_api::router(Arc::clone(&state));
    let body = json!({ "mode": "text", "text": "创建目标：整理桌面" }).to_string();
    let (status, value) = call(&app, "POST", "/command/run", Some(&body)).await;
    assert_eq!(status, 200, "{value}");
    assert_eq!(value["intent"], "create_goal");
    let goal_id = value["results"]["goal_id"].as_str().unwrap();
    assert!(!goal_id.is_empty());
    // 落盘与 goal_api 存储约定兼容。
    let goals_dir = state.data_root.join("goals");
    assert!(goals_dir.join(goal_id).join("goal.json").exists());
}

#[tokio::test]
async fn command_run_query_note_returns_hits() {
    let (state, _temp) = test_state().await;
    // 预置一篇笔记。
    let notes_dir = state.data_root.join("notes").join("doc-1");
    std::fs::create_dir_all(&notes_dir).unwrap();
    let mut doc = owo_agent_core::notes::new_doc("doc-1", "会议纪要");
    let root = doc.root.clone();
    owo_agent_core::notes::add_block(
        &mut doc,
        &root,
        owo_agent_core::notes::BlockKind::Paragraph {
            text: "本周讨论 Rust 并发编程".to_string(),
        },
        Default::default(),
    )
    .unwrap();
    owo_agent_core::notes::save_doc(&doc, &notes_dir).unwrap();

    let app = intent_api::router(Arc::clone(&state));
    let body = json!({ "mode": "text", "text": "查笔记：并发" }).to_string();
    let (status, value) = call(&app, "POST", "/command/run", Some(&body)).await;
    assert_eq!(status, 200, "{value}");
    assert_eq!(value["intent"], "query_note");
    assert_eq!(value["results"]["count"].as_u64().unwrap(), 1);
    assert_eq!(value["results"]["hits"][0]["title"], "会议纪要");
}

#[tokio::test]
async fn command_run_search_memory_works() {
    let (state, _temp) = test_state().await;
    {
        let mut memory = state.memory.lock().unwrap();
        memory
            .append(owo_agent_core::Observation {
                ts: "2026-08-14T10:00:00Z".to_string(),
                app_id: "qq".to_string(),
                kind: "sim_event".to_string(),
                summary: "张子豪约定开会".to_string(),
                detail: json!({}),
                state_hash: 7,
            })
            .unwrap();
    }
    let app = intent_api::router(Arc::clone(&state));
    let body = json!({ "mode": "text", "text": "搜索记忆：张子豪" }).to_string();
    let (status, value) = call(&app, "POST", "/command/run", Some(&body)).await;
    assert_eq!(status, 200, "{value}");
    assert_eq!(value["intent"], "search_memory");
    assert!(value["results"]["count"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn command_run_desktop_action_blocked_by_default() {
    let (state, _temp) = test_state().await;
    let app = intent_api::router(Arc::clone(&state));
    let body = json!({ "mode": "text", "text": "点击：发送按钮" }).to_string();
    let (status, value) = call(&app, "POST", "/command/run", Some(&body)).await;
    assert_eq!(status, 200, "{value}");
    assert_eq!(value["intent"], "desktop_action");
    assert_eq!(value["results"]["blocked"], true);
    assert!(value["results"]["reason"]
        .as_str()
        .unwrap()
        .contains("默认拒绝"));
}

#[tokio::test]
async fn command_run_voice_missing_wav_400() {
    let (state, _temp) = test_state().await;
    let app = intent_api::router(Arc::clone(&state));
    let body = json!({ "mode": "voice", "text": "创建目标" }).to_string();
    let (status, value) = call(&app, "POST", "/command/run", Some(&body)).await;
    assert_eq!(status, 400, "{value}");
    assert!(value["error"].as_str().unwrap().contains("wav_b64"));
}

#[tokio::test]
async fn command_run_voice_invalid_wav_400() {
    let (state, _temp) = test_state().await;
    let app = intent_api::router(Arc::clone(&state));
    let wav = base64::engine::general_purpose::STANDARD.encode(b"not-a-wav");
    let body = json!({ "mode": "voice", "wav_b64": wav }).to_string();
    let (status, value) = call(&app, "POST", "/command/run", Some(&body)).await;
    assert_eq!(status, 400, "{value}");
    assert!(value["error"].as_str().unwrap().contains("WAV"));
}

#[tokio::test]
async fn command_run_run_workflow_demo_executes() {
    let (state, _temp) = test_state().await;
    let app = intent_api::router(Arc::clone(&state));
    let body = json!({ "mode": "text", "text": "运行工作流：演示流程" }).to_string();
    let (status, value) = call(&app, "POST", "/command/run", Some(&body)).await;
    assert_eq!(status, 200, "{value}");
    assert_eq!(value["intent"], "run_workflow");
    assert_eq!(value["results"]["workflow"], "演示流程");
    let state_str = value["results"]["state"].as_str().unwrap();
    assert!(
        state_str == "Succeeded" || state_str == "Failed",
        "演示流程应执行到终态：{value}"
    );
}

#[tokio::test]
async fn command_audit_tail_nonempty() {
    let (state, _temp) = test_state().await;
    let app = intent_api::router(Arc::clone(&state));
    let body = json!({ "mode": "text", "text": "创建目标：审计测试" }).to_string();
    let (status, _) = call(&app, "POST", "/command/run", Some(&body)).await;
    assert_eq!(status, 200);
    let (_, audit) = call(&app, "GET", "/command/audit", None).await;
    let entries = audit["audit"].as_array().unwrap();
    assert!(!entries.is_empty());
    let text = serde_json::to_string(&audit).unwrap();
    assert!(text.contains("command.run"), "审计应含命令记录");
}
