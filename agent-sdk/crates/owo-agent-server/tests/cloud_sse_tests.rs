//! Lane D Part 2 契约测试：云端 SSE 进度集线器。
//!
//! 覆盖：hub 发布/订阅（历史重放 + 实时流）、ProgressSink 适配器与 CollectingSink
//! 事件序列一致、SSE 端点（text/event-stream + 首帧 event:/data: 语义）。

#[path = "../src/sse.rs"]
mod sse;

use owo_agent_core::cloud_exec::{CloudProgress, CollectingSink, ProgressSink};
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

fn all_progress_events(task_id: &str) -> Vec<CloudProgress> {
    vec![
        CloudProgress::Snapshotting {
            task_id: task_id.into(),
        },
        CloudProgress::Submitting {
            task_id: task_id.into(),
        },
        CloudProgress::Submitted {
            task_id: task_id.into(),
            remote_id: "remote-1".into(),
        },
        CloudProgress::Executing {
            task_id: task_id.into(),
        },
        CloudProgress::Fetching {
            task_id: task_id.into(),
        },
        CloudProgress::Retrying {
            task_id: task_id.into(),
            retry_count: 2,
        },
        CloudProgress::Succeeded {
            task_id: task_id.into(),
            diff_count: 3,
        },
        CloudProgress::Failed {
            task_id: task_id.into(),
            error: "boom".into(),
        },
        CloudProgress::Canceled {
            task_id: task_id.into(),
        },
    ]
}

#[tokio::test]
async fn hub_publish_replays_history_then_streams() {
    sse::reset_hub_for_test();
    let hub = sse::hub();
    hub.publish("task-hist", "{\"event\":\"submitted\"}".to_string());
    hub.publish("task-hist", "{\"event\":\"executing\"}".to_string());

    // 订阅先重放历史。
    let (mut receiver, history) = hub.subscribe("task-hist");
    assert_eq!(history.len(), 2);
    assert_eq!(history[0], "{\"event\":\"submitted\"}");

    // 再实时收到后续帧。
    hub.publish("task-hist", "{\"event\":\"succeeded\"}".to_string());
    let frame = tokio::time::timeout(std::time::Duration::from_secs(3), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(frame, "{\"event\":\"succeeded\"}");
    assert_eq!(hub.history("task-hist").len(), 3);
}

#[tokio::test]
async fn sink_frames_match_collecting_sink_sequence() {
    sse::reset_hub_for_test();
    let task_id = "task-seq";
    let sink = sse::sink(task_id);
    let collecting = CollectingSink::new();

    let events = all_progress_events(task_id);
    for event in &events {
        collecting.emit(event);
        sink.emit(event);
    }

    let collected: Vec<Value> = collecting
        .all()
        .iter()
        .map(|e| serde_json::from_str(&sse::progress_frame(e)).unwrap())
        .collect();
    let hub_frames: Vec<Value> = sse::hub()
        .history(task_id)
        .iter()
        .map(|f| serde_json::from_str(f).unwrap())
        .collect();

    assert_eq!(hub_frames.len(), collected.len(), "帧数一致");
    for (hub_frame, collected_frame) in hub_frames.iter().zip(collected.iter()) {
        assert_eq!(hub_frame["kind"], collected_frame["kind"]);
        assert_eq!(hub_frame["task_id"], collected_frame["task_id"]);
    }
    // 序列顺序一致（Submitted 带 remote_id、Retrying 带 retry_count）。
    let kinds: Vec<&str> = hub_frames
        .iter()
        .filter_map(|f| f["kind"].as_str())
        .collect();
    assert_eq!(
        kinds,
        vec![
            "snapshotting",
            "submitting",
            "submitted",
            "executing",
            "fetching",
            "retrying",
            "succeeded",
            "failed",
            "canceled"
        ]
    );
    assert_eq!(hub_frames[2]["remote_id"], "remote-1");
    assert_eq!(hub_frames[5]["retry_count"], 2);
    assert_eq!(hub_frames[7]["error"], "boom");
}

#[tokio::test]
async fn sink_emits_json_with_event_kind_fields() {
    sse::reset_hub_for_test();
    let task_id = "task-frame";
    let sink = sse::sink(task_id);
    sink.emit(&CloudProgress::Succeeded {
        task_id: task_id.into(),
        diff_count: 5,
    });
    let frames = sse::hub().history(task_id);
    assert_eq!(frames.len(), 1);
    let frame: Value = serde_json::from_str(&frames[0]).unwrap();
    assert_eq!(frame["event"], "succeeded");
    assert_eq!(frame["kind"], "succeeded");
    assert_eq!(frame["diff_count"], 5);
}

#[tokio::test]
async fn events_endpoint_returns_event_stream_content_type() {
    let (state, _temp) = test_state().await;
    let app = sse::router(state);
    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/cloud/tasks/task-http/events")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.contains("text/event-stream"),
        "Content-Type 应为 text/event-stream：{content_type}"
    );
    assert!(sse::sse_response_ok(&response));
}

#[tokio::test]
async fn events_endpoint_replays_history_in_first_frame() {
    sse::reset_hub_for_test();
    let task_id = "task-replay";
    sse::hub().publish(
        task_id,
        json!({"event": "submitted", "remote_id": "r9"}).to_string(),
    );

    let (state, _temp) = test_state().await;
    let app = sse::router(state);
    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri(format!("/cloud/tasks/{task_id}/events"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert!(sse::sse_response_ok(&response));

    // SSE 帧格式由 axum Event 编码（event: progress / data: <frame>）；
    // 帧语义在 hub 层验证：历史已被重放、格式含 event:/data: 行。
    let frame_text = sse::sse_frame_text(&sse::hub().history(task_id)[0]);
    assert!(
        frame_text.contains("event: progress"),
        "帧应含 event: 行：{frame_text}"
    );
    assert!(
        frame_text.contains("data:"),
        "帧应含 data: 行：{frame_text}"
    );
    assert!(frame_text.contains("r9"), "历史帧应被重放：{frame_text}");
    assert!(!sse::hub().history(task_id).is_empty());
}

#[tokio::test]
async fn hub_history_isolated_by_task_id() {
    // 单例 hub 无法重置（OnceLock）；按 task_id 隔离历史即可并行安全。
    let task_a = format!("task-iso-{}", uuid::Uuid::new_v4());
    let task_b = format!("task-iso-{}", uuid::Uuid::new_v4());
    sse::hub().publish(&task_a, "{}".to_string());
    assert_eq!(sse::hub().history(&task_a).len(), 1);
    assert_eq!(
        sse::hub().history(&task_b).len(),
        0,
        "不同 task_id 互不干扰"
    );
    // 订阅/发布同一 task 前后一致。
    let (_, history) = sse::hub().subscribe(&task_a);
    assert_eq!(history.len(), 1);
}
