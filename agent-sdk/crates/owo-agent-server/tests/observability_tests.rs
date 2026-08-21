//! 可观测性测试（R5 Agent 3 子任务 2）：/metrics/*。
//!
//! 独立编译：`#[path = "../src/observability_api.rs"] mod observability_api;`。
//! 全部使用 tempdir，写入 TraceRecord 后断言聚合统计。

use owo_agent_core::permissions::Policy;
use owo_agent_core::sqlite_store::SqliteSessionStore;
use owo_agent_core::tools::ToolRegistry;
use owo_agent_core::{save_trace, Agent, TokenUsage, TraceRecord, TurnEvent};
use std::sync::Arc;
use tower::ServiceExt;

#[path = "../src/observability_api.rs"]
mod observability_api;

#[path = "../src/event_stream.rs"]
mod event_stream;

#[path = "../src/slo.rs"]
mod slo;

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

fn trace_record(
    session_id: &str,
    started_at: &str,
    duration_ms: u64,
    steps: usize,
    events: Vec<TurnEvent>,
) -> TraceRecord {
    TraceRecord {
        session_id: session_id.to_string(),
        workspace: "ws".to_string(),
        model: "test-model".to_string(),
        prompt: format!("prompt-{session_id}"),
        started_at: started_at.to_string(),
        duration_ms,
        steps,
        final_text: None,
        events,
        usage: TokenUsage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        },
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
    let traces_dir = temp.path().join("traces");
    let state = Arc::new(owo_agent_server::AppState::new(
        agent,
        store,
        traces_dir,
        temp.path().to_path_buf(),
        workspace,
    ));
    (state, temp)
}

fn request(method: &str, path: &str) -> axum::http::Request<axum::body::Body> {
    use axum::http::{Method, Request};
    Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(path)
        .body(axum::body::Body::empty())
        .unwrap()
}

async fn send(
    state: Arc<owo_agent_server::AppState>,
    method: &str,
    path: &str,
) -> axum::http::Response<axum::body::Body> {
    observability_api::router(state)
        .oneshot(request(method, path))
        .await
        .unwrap()
}

async fn body_json(response: axum::http::Response<axum::body::Body>) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// 写入 5 条 trace：耗时 100/200/300/400/1000 → p50=300、p95=1000、avg=400。
async fn seed_five_traces(state: &Arc<owo_agent_server::AppState>) {
    let durations = [100u64, 200, 300, 400, 1000];
    for (i, duration) in durations.iter().enumerate() {
        let events = vec![
            TurnEvent::ToolStart {
                id: format!("t{i}"),
                tool: "read_file".to_string(),
            },
            TurnEvent::ToolResult {
                id: format!("t{i}"),
                tool: "read_file".to_string(),
                ok: true,
                error: None,
            },
            TurnEvent::ToolStart {
                id: format!("w{i}"),
                tool: "write_file".to_string(),
            },
            TurnEvent::ToolResult {
                id: format!("w{i}"),
                tool: "write_file".to_string(),
                ok: i % 3 != 0,
                error: (i % 3 == 0).then(|| "denied".to_string()),
            },
        ];
        let record = trace_record(
            &format!("sess-{i}"),
            &format!("2026-08-15T00:0{i}:00Z"),
            *duration,
            2,
            events,
        );
        save_trace(&state.traces_dir, &record).unwrap();
    }
}

#[tokio::test]
async fn overview_aggregates_counts_and_percentiles() {
    let (state, _temp) = test_state().await;
    seed_five_traces(&state).await;

    let response = send(state, "GET", "/metrics/overview").await;
    assert_eq!(response.status().as_u16(), 200);
    let body = body_json(response).await;
    assert_eq!(body["traces_count"], serde_json::json!(5));
    assert_eq!(
        body["p50_ms"],
        serde_json::json!(300),
        "排序后 100,200,300,400,1000 的 p50"
    );
    assert_eq!(body["p95_ms"], serde_json::json!(1000));
    assert_eq!(body["avg_turn_ms"], serde_json::json!(400.0));
    // 工具调用：5 × read_file(成功) + 5 × write_file(失败 2 次：i=0,3)
    assert_eq!(body["tool_calls_total"], serde_json::json!(10));
    assert_eq!(body["failures"], serde_json::json!(2));
    assert!(body["updated_at"].as_str().is_some());
}

#[tokio::test]
async fn overview_approvals_from_audit_log() {
    let (state, _temp) = test_state().await;
    {
        let log = state.agent.audit_log();
        let mut audit = log.lock().unwrap();
        audit.record(
            "s1",
            "permission",
            Some("read_file".to_string()),
            Some(true),
            "allowed",
        );
        audit.record(
            "s1",
            "permission",
            Some("write_file".to_string()),
            Some(false),
            "denied",
        );
        audit.record("s1", "turn", Some("model".to_string()), None, "unrelated");
    }
    let response = send(state, "GET", "/metrics/overview").await;
    assert_eq!(response.status().as_u16(), 200);
    let body = body_json(response).await;
    assert_eq!(body["approvals_total"], serde_json::json!(2));
    assert_eq!(body["denied"], serde_json::json!(1));
}

#[tokio::test]
async fn overview_empty_traces_no_panic() {
    let (state, _temp) = test_state().await;
    let response = send(state, "GET", "/metrics/overview").await;
    assert_eq!(response.status().as_u16(), 200);
    let body = body_json(response).await;
    assert_eq!(body["traces_count"], serde_json::json!(0));
    assert_eq!(body["avg_turn_ms"], serde_json::json!(0.0));
    assert_eq!(body["p50_ms"], serde_json::Value::Null);
    assert_eq!(body["tool_calls_total"], serde_json::json!(0));
}

#[tokio::test]
async fn tools_ranking_and_failure_rate() {
    let (state, _temp) = test_state().await;
    seed_five_traces(&state).await;

    let response = send(state, "GET", "/metrics/tools").await;
    assert_eq!(response.status().as_u16(), 200);
    let body = body_json(response).await;
    let tools = body["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
    // 调用数相同(5)，按 tool 名排序：read_file 在前。
    assert_eq!(tools[0]["tool"], serde_json::json!("read_file"));
    assert_eq!(tools[0]["calls"], serde_json::json!(5));
    assert_eq!(tools[0]["failures"], serde_json::json!(0));
    assert_eq!(tools[0]["failure_rate"], serde_json::json!(0.0));
    assert_eq!(tools[1]["tool"], serde_json::json!("write_file"));
    assert_eq!(tools[1]["failures"], serde_json::json!(2));
    assert!((tools[1]["failure_rate"].as_f64().unwrap() - 0.4).abs() < 1e-9);
}

#[tokio::test]
async fn turns_sorted_newest_first_with_limit() {
    let (state, _temp) = test_state().await;
    seed_five_traces(&state).await;

    let response = send(state, "GET", "/metrics/turns?limit=3").await;
    assert_eq!(response.status().as_u16(), 200);
    let body = body_json(response).await;
    let turns = body["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 3);
    // started_at 倒序：00:04 > 00:03 > 00:02
    assert_eq!(
        turns[0]["started_at"],
        serde_json::json!("2026-08-15T00:04:00Z")
    );
    assert_eq!(
        turns[2]["started_at"],
        serde_json::json!("2026-08-15T00:02:00Z")
    );
    assert_eq!(body["count"], serde_json::json!(3));
}

#[tokio::test]
async fn health_components_reflect_state() {
    let _env_guard = ENV_TEST_LOCK.lock().await;
    let (state, temp) = test_state().await;
    // notes：写一个 doc.json
    let note_dir = temp.path().join("notes").join("note-1");
    std::fs::create_dir_all(&note_dir).unwrap();
    std::fs::write(
        note_dir.join("doc.json"),
        r#"{"id":"note-1","title":"t","root":"r","blocks":{}}"#,
    )
    .unwrap();
    // plugins：workspace/plugins/p1/manifest.json
    let plugin_dir = state.workspace.join("plugins").join("p1");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.json"),
        r#"{"id":"p1","name":"p1","version":"0.1.0"}"#,
    )
    .unwrap();

    let response = send(state.clone(), "GET", "/metrics/health").await;
    assert_eq!(response.status().as_u16(), 200);
    let body = body_json(response).await;
    let components = &body["components"];
    // 测试环境无 STT 模型文件 → not ready
    assert_eq!(components["stt"]["ready"], serde_json::json!(false));
    // 无 OWO_CLOUD_BASE_URL → mock
    assert_eq!(
        components["cloud_transport"]["kind"],
        serde_json::json!("mock")
    );
    assert_eq!(components["plugins"]["count"], serde_json::json!(1));
    assert_eq!(components["notes"]["count"], serde_json::json!(1));
    assert_eq!(components["traces"]["count"], serde_json::json!(0));
}

#[tokio::test]
async fn health_with_cloud_env_reports_http() {
    let _env_guard = ENV_TEST_LOCK.lock().await;
    let (state, _temp) = test_state().await;
    std::env::set_var("OWO_CLOUD_BASE_URL", "http://127.0.0.1:9");
    let response = send(state, "GET", "/metrics/health").await;
    std::env::remove_var("OWO_CLOUD_BASE_URL");
    assert_eq!(response.status().as_u16(), 200);
    let body = body_json(response).await;
    assert_eq!(
        body["components"]["cloud_transport"]["kind"],
        serde_json::json!("http")
    );
}

#[tokio::test]
async fn turns_empty_dir_ok() {
    let (state, _temp) = test_state().await;
    let response = send(state, "GET", "/metrics/turns").await;
    assert_eq!(response.status().as_u16(), 200);
    let body = body_json(response).await;
    assert_eq!(body["count"], serde_json::json!(0));
}

// ---------- R6 Wave 1：/metrics/runtime 韧性指标 ----------

/// 运行时指标注册表是进程级静态：新指标测试串行执行，避免并行竞争。
static RUNTIME_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// OWO_CLOUD_BASE_URL 是进程级环境变量：依赖它的测试串行执行，避免读写竞争。
static ENV_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn runtime_empty_data_no_panic() {
    let _guard = RUNTIME_TEST_LOCK.lock().await;
    observability_api::reset_runtime_metrics_for_test();
    let (state, _temp) = test_state().await;
    let response = send(state, "GET", "/metrics/runtime").await;
    assert_eq!(response.status().as_u16(), 200);
    let body = body_json(response).await;
    // 空数据：tool p95 为 null、计数为 0、审批率 null，均不 panic。
    assert_eq!(body["tool"]["samples"], serde_json::json!(0));
    assert_eq!(body["tool"]["p95_ms"], serde_json::Value::Null);
    assert_eq!(body["approval"]["total"], serde_json::json!(0));
    assert_eq!(body["approval"]["pass_rate"], serde_json::Value::Null);
    assert_eq!(body["approval"]["intercept_rate"], serde_json::Value::Null);
    assert_eq!(body["queue_depth"], serde_json::json!(0));
    assert_eq!(body["sse"]["active_connections"], serde_json::json!(0));
    assert_eq!(body["events"]["published"], serde_json::json!(0));
    assert!(body["updated_at"].as_str().is_some());
}

#[tokio::test]
async fn runtime_tool_p95_computed_from_registry() {
    let _guard = RUNTIME_TEST_LOCK.lock().await;
    observability_api::reset_runtime_metrics_for_test();
    let (state, _temp) = test_state().await;
    for ms in [100u64, 200, 300, 400, 1000] {
        observability_api::record_tool_duration_ms(ms);
    }
    let response = send(state, "GET", "/metrics/runtime").await;
    assert_eq!(response.status().as_u16(), 200);
    let body = body_json(response).await;
    assert_eq!(body["tool"]["samples"], serde_json::json!(5));
    assert_eq!(body["tool"]["p95_ms"], serde_json::json!(1000));
    assert_eq!(body["tool"]["p50_ms"], serde_json::json!(300));
}

#[tokio::test]
async fn runtime_approval_pass_and_intercept_rates() {
    let _guard = RUNTIME_TEST_LOCK.lock().await;
    observability_api::reset_runtime_metrics_for_test();
    let (state, _temp) = test_state().await;
    {
        let log = state.agent.audit_log();
        let mut audit = log.lock().unwrap();
        for i in 0..4 {
            audit.record(
                "s1",
                "permission",
                Some("tool".to_string()),
                Some(i < 3),
                "audited",
            );
        }
    }
    let response = send(state, "GET", "/metrics/runtime").await;
    assert_eq!(response.status().as_u16(), 200);
    let body = body_json(response).await;
    assert_eq!(body["approval"]["total"], serde_json::json!(4));
    assert_eq!(body["approval"]["approved"], serde_json::json!(3));
    assert_eq!(body["approval"]["denied"], serde_json::json!(1));
    assert!((body["approval"]["pass_rate"].as_f64().unwrap() - 0.75).abs() < 1e-9);
    assert!((body["approval"]["intercept_rate"].as_f64().unwrap() - 0.25).abs() < 1e-9);
}

#[tokio::test]
async fn runtime_sse_connections_and_queue_depth() {
    let _guard = RUNTIME_TEST_LOCK.lock().await;
    observability_api::reset_runtime_metrics_for_test();
    let (state, _temp) = test_state().await;
    observability_api::record_sse_connection(1);
    observability_api::record_sse_connection(1);
    observability_api::record_queue_depth(512);
    let response = send(state.clone(), "GET", "/metrics/runtime").await;
    let body = body_json(response).await;
    assert_eq!(body["sse"]["active_connections"], serde_json::json!(2));
    assert_eq!(body["sse"]["total_connections"], serde_json::json!(2));
    assert_eq!(body["queue_depth"], serde_json::json!(512));
    // 断开后活跃数下降、总量保留。
    observability_api::record_sse_connection(-1);
    let response = send(state, "GET", "/metrics/runtime").await;
    let body = body_json(response).await;
    assert_eq!(body["sse"]["active_connections"], serde_json::json!(1));
    assert_eq!(body["sse"]["total_connections"], serde_json::json!(2));
}

#[tokio::test]
async fn runtime_event_stream_counters() {
    let _guard = RUNTIME_TEST_LOCK.lock().await;
    observability_api::reset_runtime_metrics_for_test();
    let (state, _temp) = test_state().await;
    observability_api::record_events(10, 2);
    observability_api::record_events(5, 1);
    let response = send(state, "GET", "/metrics/runtime").await;
    let body = body_json(response).await;
    assert_eq!(body["events"]["published"], serde_json::json!(15));
    assert_eq!(body["events"]["dropped"], serde_json::json!(3));
}

// ---------- R7 Wave 2：指标桥接 ingest_metrics_sample + /metrics/slo ----------

#[tokio::test]
async fn runtime_ingest_connection_samples_produce_real_values() {
    let _guard = RUNTIME_TEST_LOCK.lock().await;
    observability_api::reset_runtime_metrics_for_test();
    let (state, _temp) = test_state().await;
    // 模拟 event_stream 指标样本（快照式 JSON 桥接）。
    observability_api::ingest_metrics_sample(&serde_json::json!({
        "conn_opened": 1, "active_connections": 1, "queue_depth": 3,
        "published": 0, "dropped_mergeable": 0, "dropped_critical": 0, "lagged": 0,
    }));
    let response = send(state, "GET", "/metrics/runtime").await;
    let body = body_json(response).await;
    assert_eq!(body["sse"]["active_connections"], serde_json::json!(1));
    assert_eq!(body["sse"]["total_connections"], serde_json::json!(1));
    assert_eq!(body["queue_depth"], serde_json::json!(3));
    assert_eq!(body["sse"]["lagged_total"], serde_json::json!(0));
}

#[tokio::test]
async fn runtime_ingest_events_and_lagged_samples() {
    let _guard = RUNTIME_TEST_LOCK.lock().await;
    observability_api::reset_runtime_metrics_for_test();
    let (state, _temp) = test_state().await;
    observability_api::ingest_metrics_sample(&serde_json::json!({
        "conn_opened": 0, "active_connections": 0, "queue_depth": 0,
        "published": 3, "dropped_mergeable": 1, "dropped_critical": 0, "lagged": 0,
    }));
    observability_api::ingest_metrics_sample(&serde_json::json!({
        "conn_opened": 0, "active_connections": 0, "queue_depth": 0,
        "published": 2, "dropped_mergeable": 0, "dropped_critical": 1, "lagged": 1,
    }));
    let response = send(state, "GET", "/metrics/runtime").await;
    let body = body_json(response).await;
    assert_eq!(body["events"]["published"], serde_json::json!(5));
    assert_eq!(body["events"]["dropped"], serde_json::json!(2));
    assert_eq!(body["sse"]["lagged_total"], serde_json::json!(1));
}

#[tokio::test]
async fn runtime_ingest_closed_connection_decrements_active() {
    let _guard = RUNTIME_TEST_LOCK.lock().await;
    observability_api::reset_runtime_metrics_for_test();
    let (state, _temp) = test_state().await;
    observability_api::ingest_metrics_sample(&serde_json::json!({
        "conn_opened": 1, "active_connections": 1, "queue_depth": 0,
        "published": 0, "dropped_mergeable": 0, "dropped_critical": 0, "lagged": 0,
    }));
    // 快照式：active_connections 直接覆盖（关闭后为 0），总量保留。
    observability_api::ingest_metrics_sample(&serde_json::json!({
        "conn_opened": 0, "active_connections": 0, "queue_depth": 0,
        "published": 0, "dropped_mergeable": 0, "dropped_critical": 0, "lagged": 0,
    }));
    let response = send(state, "GET", "/metrics/runtime").await;
    let body = body_json(response).await;
    assert_eq!(body["sse"]["active_connections"], serde_json::json!(0));
    assert_eq!(body["sse"]["total_connections"], serde_json::json!(1));
}

#[tokio::test]
async fn slo_endpoint_returns_report_with_five_baselines() {
    let _guard = RUNTIME_TEST_LOCK.lock().await;
    observability_api::reset_runtime_metrics_for_test();
    observability_api::reset_slo_report_probe_for_test();
    slo::reset_global_for_test();
    observability_api::register_slo_report_probe(Arc::new(slo::report_global));
    let (state, _temp) = test_state().await;
    let response = send(state, "GET", "/metrics/slo").await;
    assert_eq!(response.status().as_u16(), 200);
    let body = body_json(response).await;
    assert_eq!(body["count"], serde_json::json!(5));
    let names: Vec<&str> = body["slo"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["name"].as_str())
        .collect();
    for expected in [
        "audit_zero_loss",
        "http_success",
        "ipc",
        "panel_wake",
        "tool_schedule",
    ] {
        assert!(names.contains(&expected), "SLO 报告缺：{expected}");
    }
    assert_eq!(body["slo"][0]["samples"], serde_json::json!(0));
}

#[tokio::test]
async fn slo_endpoint_without_probe_returns_empty_no_panic() {
    let _guard = RUNTIME_TEST_LOCK.lock().await;
    observability_api::reset_runtime_metrics_for_test();
    observability_api::reset_slo_report_probe_for_test();
    let (state, _temp) = test_state().await;
    let response = send(state, "GET", "/metrics/slo").await;
    assert_eq!(response.status().as_u16(), 200);
    let body = body_json(response).await;
    assert_eq!(body["count"], serde_json::json!(0));
    assert_eq!(body["slo"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn slo_endpoint_reflects_recorded_observations() {
    let _guard = RUNTIME_TEST_LOCK.lock().await;
    observability_api::reset_runtime_metrics_for_test();
    observability_api::reset_slo_report_probe_for_test();
    slo::reset_global_for_test();
    observability_api::register_slo_report_probe(Arc::new(slo::report_global));
    let (state, _temp) = test_state().await;
    slo::check_slo_global("ipc", Some(4), true);
    slo::check_slo_global("ipc", Some(9), true); // 违规
    let response = send(state, "GET", "/metrics/slo").await;
    let body = body_json(response).await;
    let ipc = body["slo"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == "ipc")
        .unwrap();
    assert_eq!(ipc["samples"], serde_json::json!(2));
    assert_eq!(ipc["p95_ms"], serde_json::json!(9));
    assert_eq!(ipc["violations"], serde_json::json!(1));
    assert!(!ipc["achieving"].as_bool().unwrap());
}

#[tokio::test]
async fn slo_endpoint_error_budget_computable() {
    let _guard = RUNTIME_TEST_LOCK.lock().await;
    observability_api::reset_runtime_metrics_for_test();
    observability_api::reset_slo_report_probe_for_test();
    slo::reset_global_for_test();
    observability_api::register_slo_report_probe(Arc::new(slo::report_global));
    let (state, _temp) = test_state().await;
    for i in 0..1000 {
        slo::check_slo_global("http_success", None, i != 700);
    }
    let response = send(state, "GET", "/metrics/slo").await;
    let body = body_json(response).await;
    let http = body["slo"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == "http_success")
        .unwrap();
    assert_eq!(http["error_budget"]["total"], serde_json::json!(1000));
    assert_eq!(http["error_budget"]["bad"], serde_json::json!(1));
    assert_eq!(http["error_budget"]["allowed_bad"], serde_json::json!(1));
    assert!(http["error_budget"]["within"].as_bool().unwrap());
    assert!(http["error_budget"]["remaining"].is_number());
}

#[tokio::test]
async fn slo_endpoint_empty_report_no_panic() {
    let _guard = RUNTIME_TEST_LOCK.lock().await;
    observability_api::reset_runtime_metrics_for_test();
    observability_api::reset_slo_report_probe_for_test();
    slo::reset_global_for_test();
    observability_api::register_slo_report_probe(Arc::new(slo::report_global));
    let (state, _temp) = test_state().await;
    let response = send(state, "GET", "/metrics/slo").await;
    assert_eq!(response.status().as_u16(), 200);
    let body = body_json(response).await;
    assert_eq!(
        body["slo"][0]["samples"],
        serde_json::json!(0),
        "空报告不 panic"
    );
    assert!(body["slo"][0]["error_budget"]["within"].as_bool().unwrap());
}

#[tokio::test]
async fn e2e_event_stream_feeds_runtime_metrics() {
    let _guard = RUNTIME_TEST_LOCK.lock().await;
    observability_api::reset_runtime_metrics_for_test();
    event_stream::reset_hub_for_test();
    event_stream::reset_metrics_observer_for_test();
    // 模拟主控接线：event_stream 指标钩子 → observability_api ingest。
    event_stream::set_metrics_observer(Box::new(|sample| {
        observability_api::ingest_metrics_sample(&sample.to_json());
    }));
    let (state, _temp) = test_state().await;
    let (subscription, _) = event_stream::hub().subscribe();
    // 模拟流量：发布 3 个事件。
    event_stream::hub().publish_progress("e1");
    event_stream::hub().publish_progress("e2");
    event_stream::hub().publish_approval("e3");
    let _ = subscription.try_recv(); // 消费一个，制造队列深度。
    let response = send(state.clone(), "GET", "/metrics/runtime").await;
    assert_eq!(response.status().as_u16(), 200);
    let body = body_json(response).await;
    assert!(
        body["events"]["published"].as_u64().unwrap() >= 3,
        "真实发布计数应为非零：{}",
        body["events"]["published"]
    );
    assert!(
        body["sse"]["active_connections"].as_u64().unwrap() >= 1,
        "真实活跃连接应为非零"
    );
    assert!(
        body["queue_depth"].as_u64().unwrap() >= 2,
        "真实队列深度应为非零"
    );
    event_stream::hub().close(&subscription);
    let response = send(state, "GET", "/metrics/runtime").await;
    let body = body_json(response).await;
    assert_eq!(body["sse"]["active_connections"], serde_json::json!(0));
    assert_eq!(body["sse"]["total_connections"], serde_json::json!(1));
    event_stream::reset_metrics_observer_for_test();
}

// ==================== R12：Prometheus 文本导出契约 ====================

async fn prometheus_text(state: Arc<owo_agent_server::AppState>) -> String {
    let response = send(state, "GET", "/metrics/prometheus").await;
    assert_eq!(response.status().as_u16(), 200);
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// 解析 Prometheus 文本中某条 metric 的 sample 值（文本合法性抽查）。
fn prom_sample(text: &str, metric: &str, label: &str) -> String {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix(metric) {
            if rest.starts_with('{') {
                if label.is_empty() || rest.contains(label) {
                    return rest.split_whitespace().last().unwrap_or("").to_string();
                }
            } else if label.is_empty() {
                return rest.split_whitespace().last().unwrap_or("").to_string();
            }
        }
    }
    panic!("metric {metric} 未出现在 prometheus 输出（{label}）")
}

#[tokio::test]
async fn prometheus_exports_red_tool_sse_queue_approval() {
    let _guard = RUNTIME_TEST_LOCK.lock().await;
    observability_api::reset_runtime_metrics_for_test();
    observability_api::record_tool_duration_ms(8);
    observability_api::record_tool_duration_ms(20);
    observability_api::record_tool_duration_ms(30);
    observability_api::record_sse_connection(1);
    observability_api::record_queue_depth(3);
    let (state, _temp) = test_state().await;
    seed_five_traces(&state).await;

    let text = prometheus_text(state).await;
    // RED：rate 近似（回合数）/ errors / duration 分位。
    assert!(text.contains("owo_turns_total 5"), "RED rate 缺失");
    assert!(text.contains("owo_errors_total "), "RED errors 缺失");
    assert!(
        text.contains("owo_turn_duration_ms{quantile=\"0.95\"} 1000"),
        "回合 p95 缺失"
    );
    // 工具 p95（运行时注册表样本 8/20/30 → p50=20、p95=30）。
    assert!(
        text.contains("owo_tool_duration_ms{quantile=\"0.95\"} 30"),
        "工具 p95 缺失"
    );
    assert!(
        text.contains("owo_tool_duration_ms{quantile=\"0.5\"} 20"),
        "工具 p50 缺失"
    );
    // SSE 连接 / 队列深度。
    assert!(
        text.contains("owo_sse_active_connections 1"),
        "SSE 活跃连接缺失"
    );
    assert!(
        text.contains("owo_sse_connections_total 1"),
        "SSE 累计连接缺失"
    );
    assert!(text.contains("owo_event_queue_depth 3"), "队列深度缺失");
    // 审批率。
    assert!(text.contains("owo_approvals_total "), "审批总数缺失");
    assert!(text.contains("owo_approval_pass_rate "), "审批率缺失");
    observability_api::reset_runtime_metrics_for_test();
}

#[tokio::test]
async fn prometheus_empty_data_emits_valid_nan_not_blank() {
    let _guard = RUNTIME_TEST_LOCK.lock().await;
    observability_api::reset_runtime_metrics_for_test();
    observability_api::reset_slo_report_probe_for_test();
    observability_api::reset_usage_probe_for_test();
    let (state, _temp) = test_state().await;
    let text = prometheus_text(state).await;
    // 空数据：每行 sample 均为合法浮点（无空白值，R11 保证）；分位输出 NaN。
    assert_eq!(
        prom_sample(&text, "owo_turn_duration_ms", "quantile=\"0.95\""),
        "NaN"
    );
    assert_eq!(
        prom_sample(&text, "owo_tool_duration_ms", "quantile=\"0.95\""),
        "NaN"
    );
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let value = line.split_whitespace().last().unwrap();
        assert!(
            value.parse::<f64>().is_ok(),
            "sample 值非法（应可解析为浮点）：{line}"
        );
    }
    observability_api::reset_runtime_metrics_for_test();
}

#[tokio::test]
async fn prometheus_slo_probe_appends_slo_metrics() {
    let _guard = RUNTIME_TEST_LOCK.lock().await;
    observability_api::reset_runtime_metrics_for_test();
    observability_api::reset_slo_report_probe_for_test();
    observability_api::register_slo_report_probe(Arc::new(slo::report_global));
    let registry = slo::global();
    // 制造一条达标 + 一条违规。
    for _ in 0..10 {
        slo::check_slo(&registry, "ipc", Some(1), true);
    }
    for _ in 0..10 {
        slo::check_slo(&registry, "http_success", Some(1), false); // 成功率违规
    }
    let (state, _temp) = test_state().await;
    let text = prometheus_text(state).await;
    assert!(
        text.contains("owo_slo_achieving{name=\"ipc\"} 1"),
        "达标 SLO 缺失"
    );
    assert!(
        text.contains("owo_slo_budget_remaining{name=\"http_success\"}"),
        "错误预算指标缺失"
    );
    observability_api::reset_slo_report_probe_for_test();
    slo::reset_global_for_test();
}

#[tokio::test]
async fn prometheus_usage_probe_appends_usage_metrics_and_hard_stop() {
    let _guard = RUNTIME_TEST_LOCK.lock().await;
    observability_api::reset_runtime_metrics_for_test();
    observability_api::reset_usage_probe_for_test();
    observability_api::register_usage_probe(Arc::new(|| {
        serde_json::json!({
            "count": 1,
            "hard_stop": true,
            "hard_stop_reason": "测试超限",
            "dimensions": [
                { "dimension": "session", "calls": 3, "total_tokens": 100, "cost_usd": 0.01,
                  "budget": { "limit_usd": 1.0, "spent_usd": 2.0, "exceeded": true } }
            ]
        })
    }));
    let (state, _temp) = test_state().await;
    let text = prometheus_text(state).await;
    assert!(
        text.contains("owo_usage_calls_total{dimension=\"session\"} 3"),
        "用量 calls 缺失"
    );
    assert!(
        text.contains("owo_usage_tokens_total{dimension=\"session\"} 100"),
        "用量 tokens 缺失"
    );
    assert!(
        text.contains("owo_usage_cost_usd_total{dimension=\"session\"} 0.01"),
        "用量成本缺失"
    );
    assert!(
        text.contains("owo_usage_budget_spent_usd{dimension=\"session\"} 2"),
        "预算花费缺失"
    );
    assert!(text.contains("owo_usage_hard_stop 1"), "硬熔断指标缺失");
    observability_api::reset_usage_probe_for_test();
}
