//! 可靠事件流契约测试（R6 Agent 4 Wave 1）：seq/Last-Event-ID 续传/心跳/背压。
//!
//! 独立编译：`#[path = "../src/event_stream.rs"] mod event_stream;`。
//! 覆盖：单调 seq、历史有界、断线续传零丢失、心跳可合并、溢出丢可合并保关键、
//! 慢消费者断开不拖垮发布方、连接计数、全局统计、SSE 端点形态。

use owo_agent_server::AppState;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

#[path = "../src/event_stream.rs"]
mod event_stream;

use event_stream::{EventStreamHub, StreamEvent};

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

/// 订阅后把历史重放 + 队列内实时事件一起取出（顺序 = replay 后接 recv）。
fn drain(
    subscription: &Arc<event_stream::Subscription>,
    replay: &[StreamEvent],
    mut extra: usize,
) -> Vec<u64> {
    let mut seqs: Vec<u64> = replay.iter().map(|e| e.seq).collect();
    while extra > 0 {
        if let Some(event) = subscription.recv_blocking(Duration::from_millis(500)) {
            seqs.push(event.seq);
        }
        extra -= 1;
    }
    seqs
}

#[tokio::test]
async fn seq_starts_at_1_and_stays_monotonic() {
    let _guard = METRICS_TEST_LOCK.lock().await;
    let hub = EventStreamHub::new();
    let s1 = hub.publish_progress("a");
    let s2 = hub.publish_progress("b");
    let s3 = hub.publish_approval("c");
    assert_eq!((s1, s2, s3), (1, 2, 3));
    assert_eq!(hub.last_seq(), 3);
}

#[tokio::test]
async fn history_is_bounded_and_replay_fits_window() {
    let _guard = METRICS_TEST_LOCK.lock().await;
    let hub = EventStreamHub::new();
    for i in 0..(event_stream::HISTORY_CAPACITY + 500) {
        hub.publish_progress(format!("e{i}"));
    }
    let (_, replay) = hub.subscribe();
    assert_eq!(replay.len(), event_stream::HISTORY_CAPACITY);
    // 最旧的 500 条已被裁剪，重放从 501 号事件开始。
    assert_eq!(replay.first().unwrap().seq, 501);
    assert_eq!(hub.stats().history_len, event_stream::HISTORY_CAPACITY);
}

#[tokio::test]
async fn subscribe_after_last_event_id_resumes_without_loss() {
    let _guard = METRICS_TEST_LOCK.lock().await;
    let hub = EventStreamHub::new();
    for i in 1..=5 {
        hub.publish_progress(format!("e{i}"));
    }
    // 断线前收到 seq=2；重连按 Last-Event-ID=2 续传。
    let (subscription, replay) = hub.subscribe_after(2);
    hub.publish_progress("e6");
    let seqs = drain(&subscription, &replay, 1);
    assert_eq!(seqs, vec![3, 4, 5, 6]);
}

#[tokio::test]
async fn subscribe_after_last_seq_receives_only_live_events() {
    let _guard = METRICS_TEST_LOCK.lock().await;
    let hub = EventStreamHub::new();
    hub.publish_progress("a");
    hub.publish_progress("b");
    let (subscription, replay) = hub.subscribe_after(2);
    assert!(replay.is_empty(), "无新历史");
    hub.publish_progress("live");
    let event = subscription
        .recv_blocking(Duration::from_millis(500))
        .unwrap();
    assert_eq!(event.seq, 3);
    assert_eq!(event.data, "live");
}

#[tokio::test]
async fn heartbeat_is_mergeable_and_non_critical() {
    let _guard = METRICS_TEST_LOCK.lock().await;
    let hub = EventStreamHub::new();
    let seq = hub.heartbeat();
    let (_, replay) = hub.subscribe();
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].seq, seq);
    assert_eq!(replay[0].kind, event_stream::KIND_HEARTBEAT);
    assert!(!replay[0].critical, "心跳可合并：背压下优先被丢弃");
}

#[tokio::test]
async fn overflow_drops_mergeable_but_keeps_critical() {
    let _guard = METRICS_TEST_LOCK.lock().await;
    let hub = EventStreamHub::new();
    let (subscription, _) = hub.subscribe_with_capacity(2, 0);
    hub.publish_progress("m1");
    hub.publish_progress("m2");
    // 队列已满（2）：可合并事件被丢弃。
    hub.publish_progress("m3");
    let (mergeable_dropped, critical_dropped) = subscription.dropped();
    assert_eq!((mergeable_dropped, critical_dropped), (1, 0));
    // 关键事件挤掉队内最旧可合并事件后入队。
    hub.publish_approval("c1");
    let (mergeable_dropped, critical_dropped) = subscription.dropped();
    assert_eq!((mergeable_dropped, critical_dropped), (3, 0));
    // 队内保留的是关键事件，按序消费验证。
    let first = subscription
        .recv_blocking(Duration::from_millis(500))
        .unwrap();
    assert_eq!(first.kind, event_stream::KIND_APPROVAL);
    assert!(first.critical);
}

#[tokio::test]
async fn slow_consumer_lagged_and_publisher_never_blocks() {
    let _guard = METRICS_TEST_LOCK.lock().await;
    let hub = EventStreamHub::new();
    let (subscription, _) = hub.subscribe_with_capacity(2, 0);
    hub.publish_progress("m1");
    hub.publish_progress("m2");
    // 队列被关键事件占满后，新关键事件无处可放 → 慢消费者断开。
    hub.publish_approval("c1");
    hub.publish_approval("c2");
    hub.publish_approval("c3");
    assert!(subscription.is_lagged(), "慢消费者应被标记 lagged 并断开");
    // 断开后发布方继续发布、绝不阻塞；事件按关键计数丢失。
    hub.publish_approval("c4");
    hub.publish_circuit("c5");
    let (_, critical_dropped) = subscription.dropped();
    assert_eq!(critical_dropped, 3, "断开后 3 个关键事件记丢失");
    assert_eq!(hub.last_seq(), 7, "发布方继续推进 seq");
}

#[tokio::test]
async fn active_connections_tracks_live_subscribers() {
    let _guard = METRICS_TEST_LOCK.lock().await;
    let hub = EventStreamHub::new();
    assert_eq!(hub.active_connections(), 0);
    let (sub1, _) = hub.subscribe();
    let (sub2, _) = hub.subscribe();
    assert_eq!(hub.active_connections(), 2);
    hub.close(&sub1);
    assert_eq!(hub.active_connections(), 1);
    let (_, _) = hub.subscribe_after(0);
    assert_eq!(hub.active_connections(), 2);
    hub.close(&sub2);
    assert_eq!(hub.active_connections(), 1);
}

#[tokio::test]
async fn queue_depth_sums_across_subscribers() {
    let _guard = METRICS_TEST_LOCK.lock().await;
    let hub = EventStreamHub::new();
    let (sub_a, _) = hub.subscribe_with_capacity(4, 0);
    let (sub_b, _) = hub.subscribe_with_capacity(2, 0);
    hub.publish_progress("e1");
    hub.publish_progress("e2");
    // a=3, b=2（b 已满，第三条对 b 丢弃）。
    hub.publish_progress("e3");
    assert_eq!(hub.total_queue_depth(), 5);
    let event_a = sub_a.recv_blocking(Duration::from_millis(500)).unwrap();
    assert_eq!(event_a.seq, 1);
    assert_eq!(hub.total_queue_depth(), 4);
    let _ = sub_b.try_recv();
    assert_eq!(hub.total_queue_depth(), 3);
}

#[tokio::test]
async fn stats_report_published_and_dropped_totals() {
    let _guard = METRICS_TEST_LOCK.lock().await;
    let hub = EventStreamHub::new();
    let (subscription, _) = hub.subscribe_with_capacity(1, 0);
    hub.publish_progress("m1");
    hub.publish_progress("m2");
    hub.publish_approval("c1");
    hub.heartbeat();
    let stats = hub.stats();
    assert_eq!(stats.last_seq, 4);
    assert_eq!(stats.published_total, 4);
    assert_eq!(stats.dropped_mergeable_total, subscription.dropped().0);
    assert_eq!(stats.active_connections, 1);
    assert_eq!(stats.queue_depth, 1);
}

#[tokio::test]
async fn events_endpoint_returns_event_stream_content_type() {
    let _guard = METRICS_TEST_LOCK.lock().await;
    event_stream::reset_hub_for_test();
    event_stream::hub().publish("progress", false, "{\"step\":1}");
    let (state, _temp) = test_state().await;
    let response = event_stream::router(state)
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/events/stream?last_event_id=0")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert!(
        event_stream::sse_response_ok(&response),
        "Content-Type 应为 text/event-stream"
    );
}

#[tokio::test]
async fn sse_frame_text_has_event_and_data_lines() {
    let _guard = METRICS_TEST_LOCK.lock().await;
    let hub = EventStreamHub::new();
    hub.publish_approval("{\"ok\":true}");
    let (_, replay) = hub.subscribe();
    let frame = event_stream::sse_frame_text(&replay[0]);
    assert!(frame.starts_with("event: approval\n"), "帧格式：{frame}");
    assert!(frame.contains("\ndata: "), "帧格式：{frame}");
    assert!(frame.contains("\"seq\":1"), "帧含单调 seq：{frame}");
}

// ---------- R7 Wave 2：指标钩子（MetricsSample 观察者） ----------

/// 指标观察者全局静态：相关测试串行执行，避免并行竞争。
static METRICS_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn metrics_observer_receives_connection_opened_and_closed() {
    let _guard = METRICS_TEST_LOCK.lock().await;
    event_stream::reset_metrics_observer_for_test();
    let samples = std::sync::Arc::new(std::sync::Mutex::new(
        Vec::<event_stream::MetricsSample>::new(),
    ));
    let observer_samples = std::sync::Arc::clone(&samples);
    event_stream::set_metrics_observer(Box::new(move |sample| {
        observer_samples.lock().unwrap().push(sample.clone());
    }));
    let hub = EventStreamHub::new();
    let (subscription, _) = hub.subscribe();
    hub.close(&subscription);
    let received = samples.lock().unwrap();
    let opened = received.iter().filter(|s| s.conn_opened > 0).count();
    let closed = received.iter().filter(|s| s.conn_closed > 0).count();
    assert_eq!(opened, 1, "连接打开应发出样本");
    assert_eq!(closed, 1, "连接关闭应发出样本");
    assert!(received.iter().any(|s| s.active_connections >= 1));
    event_stream::reset_metrics_observer_for_test();
}

#[tokio::test]
async fn metrics_observer_receives_published_samples() {
    let _guard = METRICS_TEST_LOCK.lock().await;
    event_stream::reset_metrics_observer_for_test();
    let published = std::sync::Arc::new(std::sync::Mutex::new(0u64));
    let observer_published = std::sync::Arc::clone(&published);
    event_stream::set_metrics_observer(Box::new(move |sample| {
        *observer_published.lock().unwrap() += sample.published;
    }));
    let hub = EventStreamHub::new();
    hub.publish_progress("e1");
    hub.publish_approval("e2");
    assert_eq!(
        *published.lock().unwrap(),
        2,
        "发布 2 个事件应累计 2 次 published 增量"
    );
    event_stream::reset_metrics_observer_for_test();
}

#[tokio::test]
async fn metrics_observer_receives_dropped_mergeable() {
    let _guard = METRICS_TEST_LOCK.lock().await;
    event_stream::reset_metrics_observer_for_test();
    let dropped = std::sync::Arc::new(std::sync::Mutex::new(0u64));
    let observer_dropped = std::sync::Arc::clone(&dropped);
    event_stream::set_metrics_observer(Box::new(move |sample| {
        *observer_dropped.lock().unwrap() += sample.dropped_mergeable;
    }));
    let hub = EventStreamHub::new();
    let (_subscription, _) = hub.subscribe_with_capacity(1, 0);
    hub.publish_progress("m1");
    hub.publish_progress("m2"); // 队列满 → 丢弃
    assert_eq!(
        *dropped.lock().unwrap(),
        1,
        "溢出丢弃应发出 dropped_mergeable 样本"
    );
    event_stream::reset_metrics_observer_for_test();
}

#[tokio::test]
async fn metrics_observer_receives_lagged_and_dropped_critical() {
    let _guard = METRICS_TEST_LOCK.lock().await;
    event_stream::reset_metrics_observer_for_test();
    let lagged = std::sync::Arc::new(std::sync::Mutex::new(0u64));
    let dropped_critical = std::sync::Arc::new(std::sync::Mutex::new(0u64));
    let observer_lagged = std::sync::Arc::clone(&lagged);
    let observer_dropped_critical = std::sync::Arc::clone(&dropped_critical);
    event_stream::set_metrics_observer(Box::new(move |sample| {
        *observer_lagged.lock().unwrap() += sample.lagged;
        *observer_dropped_critical.lock().unwrap() += sample.dropped_critical;
    }));
    let hub = EventStreamHub::new();
    let (subscription, _) = hub.subscribe_with_capacity(1, 0);
    hub.publish_approval("c1"); // 占满
    hub.publish_approval("c2"); // 放不下 → lagged 断开
    hub.publish_approval("c3"); // lagged 后续 → dropped_critical
    assert!(subscription.is_lagged());
    assert_eq!(*lagged.lock().unwrap(), 1, "慢消费者断开应发出 lagged 样本");
    assert_eq!(
        *dropped_critical.lock().unwrap(),
        2,
        "c2 触发 lagged 丢 1 + c3 在 lagged 后按关键事件丢 1"
    );
    event_stream::reset_metrics_observer_for_test();
}

#[tokio::test]
async fn metrics_observer_receives_queue_depth_and_active_snapshot() {
    let _guard = METRICS_TEST_LOCK.lock().await;
    event_stream::reset_metrics_observer_for_test();
    let depths = std::sync::Arc::new(std::sync::Mutex::new(Vec::<usize>::new()));
    let actives = std::sync::Arc::new(std::sync::Mutex::new(Vec::<usize>::new()));
    let observer_depths = std::sync::Arc::clone(&depths);
    let observer_actives = std::sync::Arc::clone(&actives);
    event_stream::set_metrics_observer(Box::new(move |sample| {
        observer_depths.lock().unwrap().push(sample.queue_depth);
        observer_actives
            .lock()
            .unwrap()
            .push(sample.active_connections);
    }));
    let hub = EventStreamHub::new();
    let (_subscription, _) = hub.subscribe_with_capacity(8, 0);
    hub.publish_progress("a");
    hub.publish_progress("b");
    hub.publish_progress("c");
    let depths = depths.lock().unwrap();
    assert!(
        depths.iter().any(|d| *d >= 3),
        "发布 3 个事件后队列深度采样应 ≥3：{depths:?}"
    );
    assert!(
        actives.lock().unwrap().iter().any(|a| *a >= 1),
        "活跃连接采样应 ≥1"
    );
    event_stream::reset_metrics_observer_for_test();
}

#[tokio::test]
async fn stats_include_connections_opened_and_lagged_totals() {
    let _guard = METRICS_TEST_LOCK.lock().await;
    let hub = EventStreamHub::new();
    let (subscription, _) = hub.subscribe_with_capacity(1, 0);
    hub.publish_approval("c1");
    hub.publish_approval("c2"); // lagged
    hub.close(&subscription);
    let stats = hub.stats();
    assert_eq!(stats.connections_opened_total, 1);
    assert_eq!(stats.lagged_total, 1);
    assert_eq!(stats.active_connections, 0, "关闭后活跃为 0");
}

#[tokio::test]
async fn reset_metrics_observer_for_test_stops_emissions() {
    let _guard = METRICS_TEST_LOCK.lock().await;
    event_stream::reset_metrics_observer_for_test();
    let counter = std::sync::Arc::new(std::sync::Mutex::new(0u64));
    let observer_counter = std::sync::Arc::clone(&counter);
    event_stream::set_metrics_observer(Box::new(move |_sample| {
        *observer_counter.lock().unwrap() += 1;
    }));
    event_stream::reset_metrics_observer_for_test();
    let hub = EventStreamHub::new();
    hub.publish_progress("no-observer");
    assert_eq!(*counter.lock().unwrap(), 0, "清空观察者后不再收到样本");
}

#[tokio::test]
async fn metrics_sample_to_json_shape() {
    let _guard = METRICS_TEST_LOCK.lock().await;
    let sample = event_stream::MetricsSample {
        conn_opened: 1,
        published: 3,
        queue_depth: 7,
        ..event_stream::MetricsSample::default()
    };
    let value = sample.to_json();
    assert_eq!(value["conn_opened"], serde_json::json!(1));
    assert_eq!(value["published"], serde_json::json!(3));
    assert_eq!(value["queue_depth"], serde_json::json!(7));
    assert_eq!(value["lagged"], serde_json::json!(0));
    assert!(value["active_connections"].is_u64(), "快照字段齐全");
}
