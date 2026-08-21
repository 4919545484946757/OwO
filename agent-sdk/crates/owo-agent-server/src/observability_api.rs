// R11:observability_api 质量收尾完成。
// R12:observability_api 完成，待主控接线（prometheus RED/工具 p95/SSE/队列/审批率 + slo/usage 探针联动，契约测试已补）
// R12:observability_api 完成，待主控接线（prometheus label 转义 + 预算上限指标）。
//! 可观测性与性能护栏（R5 Agent 3 子任务 2 + R6 Wave 1 + R7 Wave 2 + R9 + R10 + R11 + R12）：`/metrics/*`。
//!
//! 数据源：`state.traces_dir`（TraceRecord 落盘）+ 内存审计面（approvals）+ 模块内
//! 运行时指标注册表（R6：工具调度延迟/队列深度/SSE 连接/事件计数；R7：lagged 计数 +
//! `ingest_metrics_sample` 消费 event_stream 指标钩子，/metrics/runtime 反映真实运行期值）。
//! R7 新增 `/metrics/slo`：SLO 报告经 `register_slo_report_probe` 注册（数据面在独立的
//! `slo.rs`，主控接线时注册 `slo::report_global`；未注册时返回空报告不 panic）。
//! R9 新增：`/metrics/prometheus`（Prometheus 文本导出）、`/metrics/slo/alerts`
//! （`register_slo_alerts_probe` → `slo::alerts_json`）、`/metrics/slo/report?days=`
//! （`register_slo_period_probe` → `slo::report_period_global`）。
//! R10 新增：prometheus 追加 SLO/用量指标（`register_usage_probe` → `usage::global().summary()`）、
//! `/metrics/telemetry/status` 可选遥测聚合器（默认关，仅聚合指标，数据字典供隐私声明引用）。
//! 本模块不引用 `crate::`/`super::`，可被测试以 `#[path] mod` 独立编译；
//! AppState 写全限定 `owo_agent_server::AppState`。

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use axum::Router;
use owo_agent_core::list_traces;
use owo_agent_core::load_trace;
use owo_agent_core::trace::TraceRecord;
use owo_agent_core::TurnEvent;
use owo_agent_server::AppState;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;

/// 加载 traces 目录全部记录（不可用路径静默跳过）。
fn load_all_traces(state: &AppState) -> Vec<TraceRecord> {
    list_traces(&state.traces_dir)
        .iter()
        .filter_map(|path| load_trace(path).ok())
        .collect()
}

/// 工具调度延迟样本上限（环形丢弃最旧）。
const TOOL_SAMPLES_CAP: usize = 1000;

/// 运行时指标注册表（R6）：由接线方（event_stream 接线、工具调度层）调用
/// `record_*` 填充；查询端空数据一律返回 null/0，不 panic。
pub struct RuntimeMetrics {
    tool_durations_ms: Mutex<Vec<u64>>,
    sse_active: AtomicU64,
    sse_total_connections: AtomicU64,
    sse_lagged: AtomicU64,
    queue_depth: AtomicU64,
    events_published: AtomicU64,
    events_dropped: AtomicU64,
}

impl Default for RuntimeMetrics {
    fn default() -> Self {
        Self {
            tool_durations_ms: Mutex::new(Vec::with_capacity(TOOL_SAMPLES_CAP)),
            sse_active: AtomicU64::new(0),
            sse_total_connections: AtomicU64::new(0),
            sse_lagged: AtomicU64::new(0),
            queue_depth: AtomicU64::new(0),
            events_published: AtomicU64::new(0),
            events_dropped: AtomicU64::new(0),
        }
    }
}

static RUNTIME: Mutex<Option<RuntimeMetrics>> = Mutex::new(None);

fn with_runtime<T>(f: impl FnOnce(&RuntimeMetrics) -> T) -> T {
    let mut guard = RUNTIME.lock().unwrap_or_else(|e| e.into_inner());
    let metrics = guard.get_or_insert_with(RuntimeMetrics::default);
    f(metrics)
}

/// 记录一次工具调度耗时（ms）。超上限时丢弃最旧样本。
#[allow(dead_code)] // 仅供接线方与 observability_tests 以 #[path] 独立编译调用；lib 目标内无引用。
pub fn record_tool_duration_ms(duration_ms: u64) {
    with_runtime(|metrics| {
        let mut samples = metrics
            .tool_durations_ms
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        samples.push(duration_ms);
        while samples.len() > TOOL_SAMPLES_CAP {
            samples.remove(0);
        }
    });
}

/// 记录 SSE 连接增减（open=+1 / close=-1）。
#[allow(dead_code)] // 仅供接线方与 observability_tests 以 #[path] 独立编译调用；lib 目标内无引用。
pub fn record_sse_connection(delta: i64) {
    with_runtime(|metrics| {
        if delta > 0 {
            metrics
                .sse_total_connections
                .fetch_add(delta as u64, Ordering::Relaxed);
        }
        let active = if delta < 0 {
            metrics
                .sse_active
                .load(Ordering::Relaxed)
                .saturating_sub(delta.unsigned_abs())
        } else {
            metrics.sse_active.load(Ordering::Relaxed) + delta as u64
        };
        metrics.sse_active.store(active, Ordering::Relaxed);
    });
}

/// 记录订阅队列深度（当前值快照）。
#[allow(dead_code)] // 仅供接线方与 observability_tests 以 #[path] 独立编译调用；lib 目标内无引用。
pub fn record_queue_depth(depth: u64) {
    with_runtime(|metrics| metrics.queue_depth.store(depth, Ordering::Relaxed));
}

/// 记录事件流发布/丢弃计数。
#[allow(dead_code)] // 仅供接线方与 observability_tests 以 #[path] 独立编译调用；lib 目标内无引用。
pub fn record_events(published: u64, dropped: u64) {
    with_runtime(|metrics| {
        metrics
            .events_published
            .fetch_add(published, Ordering::Relaxed);
        metrics.events_dropped.fetch_add(dropped, Ordering::Relaxed);
    });
}

/// 记录慢消费者（lagged）断开次数。
#[allow(dead_code)] // 仅供接线方与 observability_tests 以 #[path] 独立编译调用；lib 目标内无引用。
pub fn record_event_lagged(count: u64) {
    with_runtime(|metrics| {
        metrics.sse_lagged.fetch_add(count, Ordering::Relaxed);
    });
}

/// 消费 event_stream 指标钩子样本（R7 桥接）：
/// 解析 `event_stream::MetricsSample::to_json()` 快照并更新运行时注册表。
/// 与 event_stream 解耦（双方互不引用类型），主控接线：`event_stream::set_metrics_observer(closure)`，
/// closure 内 `observability_api::ingest_metrics_sample(&sample.to_json())`。
#[allow(dead_code)] // 仅供接线方与 observability_tests 以 #[path] 独立编译调用；lib 目标内无引用。
pub fn ingest_metrics_sample(sample: &Value) {
    with_runtime(|metrics| {
        if let Some(v) = sample.get("conn_opened").and_then(|v| v.as_u64()) {
            metrics
                .sse_total_connections
                .fetch_add(v, Ordering::Relaxed);
        }
        if let Some(v) = sample.get("active_connections").and_then(|v| v.as_u64()) {
            metrics.sse_active.store(v, Ordering::Relaxed);
        }
        if let Some(v) = sample.get("published").and_then(|v| v.as_u64()) {
            metrics.events_published.fetch_add(v, Ordering::Relaxed);
        }
        let dropped_mergeable = sample
            .get("dropped_mergeable")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let dropped_critical = sample
            .get("dropped_critical")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let dropped = dropped_mergeable + dropped_critical;
        if dropped > 0 {
            metrics.events_dropped.fetch_add(dropped, Ordering::Relaxed);
        }
        if let Some(v) = sample.get("lagged").and_then(|v| v.as_u64()) {
            metrics.sse_lagged.fetch_add(v, Ordering::Relaxed);
        }
        if let Some(v) = sample.get("queue_depth").and_then(|v| v.as_u64()) {
            metrics.queue_depth.store(v, Ordering::Relaxed);
        }
    });
}

/// 仅供测试：重置运行时指标注册表（进程内跨测试隔离）。
#[allow(dead_code)] // 仅供 observability_tests 以 #[path] 独立编译调用；lib 目标内无引用。
pub fn reset_runtime_metrics_for_test() {
    *RUNTIME.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// 有序样本的百分位（0.0-1.0）。空样本返回 None。
fn percentile(sorted: &[u64], p: f64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    if sorted.len() == 1 {
        return Some(sorted[0]);
    }
    let index = (((sorted.len() - 1) as f64) * p).round() as usize;
    Some(sorted[index])
}

/// 从 TraceRecord 事件流聚合工具调用与失败。
fn aggregate_events(traces: &[TraceRecord]) -> (usize, usize, HashMap<String, (usize, usize)>) {
    let mut tool_calls = 0usize;
    let mut failures = 0usize;
    let mut tools: HashMap<String, (usize, usize)> = HashMap::new(); // tool -> (calls, fails)
    for trace in traces {
        for event in &trace.events {
            match event {
                TurnEvent::ToolStart { tool, .. } => {
                    tool_calls += 1;
                    tools.entry(tool.clone()).or_insert((0, 0)).0 += 1;
                }
                TurnEvent::ToolResult {
                    tool, ok: false, ..
                } => {
                    failures += 1;
                    tools.entry(tool.clone()).or_insert((0, 0)).1 += 1;
                }
                _ => {}
            }
        }
    }
    (tool_calls, failures, tools)
}

/// 从内存审计面统计审批：approvals_total = 全部审批请求，denied = 其中拒绝数。
fn approval_stats(state: &AppState) -> (usize, usize) {
    let mut approved = 0usize;
    let mut denied = 0usize;
    if let Ok(audit) = state.agent.audit_log().lock() {
        for entry in &audit.entries {
            if entry.event.contains("permission") || entry.tool.as_deref() == Some("approver") {
                match entry.approved {
                    Some(true) => approved += 1,
                    Some(false) => denied += 1,
                    None => {}
                }
            }
        }
    }
    (approved + denied, denied)
}

/// GET /metrics/overview：聚合概览。
async fn overview(State(state): State<Arc<AppState>>) -> ApiResult {
    let traces = load_all_traces(&state);
    let mut durations: Vec<u64> = traces.iter().map(|t| t.duration_ms).collect();
    durations.sort_unstable();
    let avg_turn_ms = if durations.is_empty() {
        0.0
    } else {
        durations.iter().sum::<u64>() as f64 / durations.len() as f64
    };
    let (tool_calls_total, failures, _tools) = aggregate_events(&traces);
    let (approvals_total, denied) = approval_stats(&state);
    Ok(Json(json!({
        "traces_count": traces.len(),
        "avg_turn_ms": (avg_turn_ms * 10.0).round() / 10.0,
        "p50_ms": percentile(&durations, 0.5),
        "p95_ms": percentile(&durations, 0.95),
        "tool_calls_total": tool_calls_total,
        "approvals_total": approvals_total,
        "denied": denied,
        "failures": failures,
        "updated_at": chrono::Utc::now().to_rfc3339(),
    })))
}

/// GET /metrics/turns?limit=：最近回合耗时序列。
async fn turns(State(state): State<Arc<AppState>>, Query(query): Query<TurnsQuery>) -> ApiResult {
    let mut traces = load_all_traces(&state);
    traces.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    let limit = query.limit.unwrap_or(50).min(500);
    let items: Vec<Value> = traces
        .iter()
        .take(limit)
        .map(|t| {
            json!({
                "started_at": t.started_at,
                "duration_ms": t.duration_ms,
                "steps": t.steps,
                "model": t.model,
                "prompt": t.prompt.chars().take(60).collect::<String>(),
                "session_id": t.session_id,
            })
        })
        .collect();
    Ok(Json(json!({ "count": items.len(), "turns": items })))
}

#[derive(Deserialize)]
struct TurnsQuery {
    #[serde(default)]
    limit: Option<usize>,
}

/// GET /metrics/tools：工具调用频次/失败率排行（按调用数倒序）。
async fn tools(State(state): State<Arc<AppState>>) -> ApiResult {
    let traces = load_all_traces(&state);
    let (_calls, _failures, tools) = aggregate_events(&traces);
    let mut items: Vec<Value> = tools
        .into_iter()
        .map(|(tool, (calls, fails))| {
            json!({
                "tool": tool,
                "calls": calls,
                "failures": fails,
                "failure_rate": if calls == 0 { 0.0 } else { fails as f64 / calls as f64 },
            })
        })
        .collect();
    items.sort_by(|a, b| {
        b["calls"]
            .as_u64()
            .cmp(&a["calls"].as_u64())
            .then_with(|| a["tool"].as_str().cmp(&b["tool"].as_str()))
    });
    Ok(Json(json!({ "count": items.len(), "tools": items })))
}

/// GET /metrics/health：组件健康清单。
async fn health(State(state): State<Arc<AppState>>) -> ApiResult {
    let stt_ready = state.stt.lock().map(|stt| stt.is_ready()).unwrap_or(false);
    let cloud_transport = std::env::var("OWO_CLOUD_BASE_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
        .map(|_| "http".to_string())
        .unwrap_or_else(|| "mock".to_string());
    let plugin_count = owo_agent_core::discover_plugins(&state.workspace, &state.data_root).len();
    let notes_dir = state.data_root.join("notes");
    let notes_count = std::fs::read_dir(&notes_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().join("doc.json").exists())
                .count()
        })
        .unwrap_or(0);
    let traces_count = list_traces(&state.traces_dir).len();
    Ok(Json(json!({
        "components": {
            "stt": { "ready": stt_ready },
            "cloud_transport": { "kind": cloud_transport },
            "plugins": { "count": plugin_count },
            "notes": { "count": notes_count },
            "traces": { "count": traces_count },
        },
    })))
}

/// SLO 报告探针：返回 `{count, slo:[...]}` JSON（数据面在 slo.rs）。
pub type SloReportProbe = Arc<dyn Fn() -> Value + Send + Sync>;

static SLO_PROBE: Mutex<Option<SloReportProbe>> = Mutex::new(None);

/// 注册 SLO 报告探针（主控接线：`register_slo_report_probe(Arc::new(slo::report_global))`）。
#[allow(dead_code)] // 仅供主控接线与 observability_tests 以 #[path] 独立编译调用。
pub fn register_slo_report_probe(probe: SloReportProbe) {
    let mut slot = SLO_PROBE.lock().unwrap_or_else(|e| e.into_inner());
    *slot = Some(probe);
}

/// 仅供测试：清空 SLO 探针。
#[allow(dead_code)] // 仅供 observability_tests 以 #[path] 独立编译调用；lib 目标内无引用。
pub fn reset_slo_report_probe_for_test() {
    let mut slot = SLO_PROBE.lock().unwrap_or_else(|e| e.into_inner());
    *slot = None;
}

/// GET /metrics/runtime（R6 Wave 1 + R7 Wave 2）：运行时韧性指标。
/// 工具调度 p95、审批通过/拦截率、队列深度、SSE 活跃连接、事件流计数；
/// 空数据返回 null/0，不 panic。
async fn runtime(State(state): State<Arc<AppState>>) -> ApiResult {
    let (
        tool_durations,
        queue_depth,
        sse_active,
        sse_total,
        sse_lagged,
        events_published,
        events_dropped,
    ) = with_runtime(|metrics| {
        let mut tool_durations = metrics
            .tool_durations_ms
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        tool_durations.sort_unstable();
        (
            tool_durations,
            metrics.queue_depth.load(Ordering::Relaxed),
            metrics.sse_active.load(Ordering::Relaxed),
            metrics.sse_total_connections.load(Ordering::Relaxed),
            metrics.sse_lagged.load(Ordering::Relaxed),
            metrics.events_published.load(Ordering::Relaxed),
            metrics.events_dropped.load(Ordering::Relaxed),
        )
    });
    let (approvals_total, denied) = approval_stats(&state);
    let approved = approvals_total.saturating_sub(denied);
    let pass_rate = if approvals_total == 0 {
        Value::Null
    } else {
        json!((approved as f64 / approvals_total as f64 * 1000.0).round() / 1000.0)
    };
    let intercept_rate = if approvals_total == 0 {
        Value::Null
    } else {
        json!((denied as f64 / approvals_total as f64 * 1000.0).round() / 1000.0)
    };
    Ok(Json(json!({
        "tool": {
            "samples": tool_durations.len(),
            "p95_ms": percentile(&tool_durations, 0.95),
            "p50_ms": percentile(&tool_durations, 0.5),
        },
        "approval": {
            "total": approvals_total,
            "approved": approved,
            "denied": denied,
            "pass_rate": pass_rate,
            "intercept_rate": intercept_rate,
        },
        "queue_depth": queue_depth,
        "sse": {
            "active_connections": sse_active,
            "total_connections": sse_total,
            "lagged_total": sse_lagged,
        },
        "events": {
            "published": events_published,
            "dropped": events_dropped,
        },
        "updated_at": chrono::Utc::now().to_rfc3339(),
    })))
}

/// GET /metrics/slo（R7 Wave 2）：SLO 注册表、错误预算、达标状态。
/// 数据面由 `register_slo_report_probe` 注册的探针提供；未注册返回空报告（不 panic）。
async fn slo(State(_state): State<Arc<AppState>>) -> ApiResult {
    let probe = SLO_PROBE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    match probe {
        Some(probe) => Ok(Json(probe())),
        None => Ok(Json(json!({
            "count": 0,
            "slo": [],
            "note": "slo report probe not registered",
        }))),
    }
}

// ==================== R9：Prometheus 文本导出 ====================

/// GET /metrics/prometheus（R9 + R10，Prometheus text exposition format）。
/// 数据面：traces（回合数/耗时/工具/错误）+ runtime（工具 p95/SSE/队列/事件）+ 审批；
/// R10：若 SLO 报告探针已注册，追加 `owo_slo_*` 指标；若用量探针已注册，追加 `owo_usage_*` 指标。
/// 响应头回填 X-Trace-Id（继承请求头，无则留空）——与 logging 贯穿约定一致。
async fn prometheus(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let body = render_prometheus(&state);
    let mut response = (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
        .into_response();
    let trace = headers
        .get("x-trace-id")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.trim().is_empty());
    if let Some(trace) = trace {
        if let Ok(value) = axum::http::HeaderValue::from_str(trace) {
            response.headers_mut().insert("x-trace-id", value);
        }
    }
    response
}

/// Prometheus 文本渲染（纯函数，供 handler 与测试复用）。
/// R11：`q()` 无样本时输出 NaN，保证每行 sample value 均为合法浮点（text exposition 格式）。
fn render_prometheus(state: &AppState) -> String {
    let traces = load_all_traces(state);
    let mut durations: Vec<u64> = traces.iter().map(|t| t.duration_ms).collect();
    durations.sort_unstable();
    let avg_turn_ms = if durations.is_empty() {
        0.0
    } else {
        durations.iter().sum::<u64>() as f64 / durations.len() as f64
    };
    let (tool_calls, failures, _tools) = aggregate_events(&traces);
    let (approvals_total, denied) = approval_stats(state);
    let approved = approvals_total.saturating_sub(denied);
    let pass_rate = if approvals_total == 0 {
        0.0
    } else {
        approved as f64 / approvals_total as f64
    };
    let (
        tool_durations,
        queue_depth,
        sse_active,
        sse_total,
        sse_lagged,
        events_published,
        events_dropped,
    ) = with_runtime(|metrics| {
        let mut tool_durations = metrics
            .tool_durations_ms
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        tool_durations.sort_unstable();
        (
            tool_durations,
            metrics.queue_depth.load(Ordering::Relaxed),
            metrics.sse_active.load(Ordering::Relaxed),
            metrics.sse_total_connections.load(Ordering::Relaxed),
            metrics.sse_lagged.load(Ordering::Relaxed),
            metrics.events_published.load(Ordering::Relaxed),
            metrics.events_dropped.load(Ordering::Relaxed),
        )
    });
    // R11：无样本时输出 NaN（Prometheus 合法值），避免空 sample value 破坏文本格式。
    let q = |sorted: &[u64], p: f64| -> String {
        percentile(sorted, p)
            .map(|v| v.to_string())
            .unwrap_or_else(|| "NaN".to_string())
    };
    let mut out = String::new();
    // RED：rate（回合总数近似）/ errors / duration。
    out.push_str("# HELP owo_turns_total 已记录回合数（traces，rate 近似）\n");
    out.push_str("# TYPE owo_turns_total counter\n");
    out.push_str(&format!("owo_turns_total {}\n", traces.len()));
    out.push_str("# HELP owo_errors_total 回合内工具失败 + 事件流丢弃（errors 近似）\n");
    out.push_str("# TYPE owo_errors_total counter\n");
    out.push_str(&format!(
        "owo_errors_total {}\n",
        failures as u64 + events_dropped
    ));
    out.push_str("# HELP owo_turn_duration_ms 回合耗时百分位（ms）\n");
    out.push_str("# TYPE owo_turn_duration_ms gauge\n");
    out.push_str(&format!(
        "owo_turn_duration_ms{{quantile=\"0.5\"}} {}\nowo_turn_duration_ms{{quantile=\"0.95\"}} {}\nowo_turn_duration_ms{{quantile=\"avg\"}} {:.1}\n",
        q(&durations, 0.5),
        q(&durations, 0.95),
        avg_turn_ms,
    ));
    // 工具。
    out.push_str("# HELP owo_tool_calls_total 工具调用总数\n");
    out.push_str("# TYPE owo_tool_calls_total counter\n");
    out.push_str(&format!("owo_tool_calls_total {tool_calls}\n"));
    out.push_str("# HELP owo_tool_failures_total 工具失败总数\n");
    out.push_str("# TYPE owo_tool_failures_total counter\n");
    out.push_str(&format!("owo_tool_failures_total {failures}\n"));
    out.push_str("# HELP owo_tool_duration_ms 工具调度耗时百分位（ms）\n");
    out.push_str("# TYPE owo_tool_duration_ms gauge\n");
    out.push_str(&format!(
        "owo_tool_duration_ms{{quantile=\"0.5\"}} {}\nowo_tool_duration_ms{{quantile=\"0.95\"}} {}\n",
        q(&tool_durations, 0.5),
        q(&tool_durations, 0.95),
    ));
    // 审批。
    out.push_str("# HELP owo_approvals_total 审批请求总数\n");
    out.push_str("# TYPE owo_approvals_total counter\n");
    out.push_str(&format!("owo_approvals_total {approvals_total}\n"));
    out.push_str("# HELP owo_approvals_denied_total 审批拒绝总数\n");
    out.push_str("# TYPE owo_approvals_denied_total counter\n");
    out.push_str(&format!("owo_approvals_denied_total {denied}\n"));
    out.push_str("# HELP owo_approval_pass_rate 审批通过率\n");
    out.push_str("# TYPE owo_approval_pass_rate gauge\n");
    out.push_str(&format!("owo_approval_pass_rate {pass_rate:.4}\n"));
    // SSE / 队列 / 事件。
    out.push_str("# HELP owo_sse_active_connections SSE 活跃连接数\n");
    out.push_str("# TYPE owo_sse_active_connections gauge\n");
    out.push_str(&format!("owo_sse_active_connections {sse_active}\n"));
    out.push_str("# HELP owo_sse_connections_total SSE 累计连接数\n");
    out.push_str("# TYPE owo_sse_connections_total counter\n");
    out.push_str(&format!("owo_sse_connections_total {sse_total}\n"));
    out.push_str("# HELP owo_sse_lagged_total 慢消费者断开次数\n");
    out.push_str("# TYPE owo_sse_lagged_total counter\n");
    out.push_str(&format!("owo_sse_lagged_total {sse_lagged}\n"));
    out.push_str("# HELP owo_event_queue_depth 事件队列总深度\n");
    out.push_str("# TYPE owo_event_queue_depth gauge\n");
    out.push_str(&format!("owo_event_queue_depth {queue_depth}\n"));
    out.push_str("# HELP owo_events_published_total 事件流发布总数\n");
    out.push_str("# TYPE owo_events_published_total counter\n");
    out.push_str(&format!("owo_events_published_total {events_published}\n"));
    out.push_str("# HELP owo_events_dropped_total 事件流丢弃总数\n");
    out.push_str("# TYPE owo_events_dropped_total counter\n");
    out.push_str(&format!("owo_events_dropped_total {events_dropped}\n"));
    // R10：SLO 指标（探针已注册时）。
    let slo_probe = SLO_PROBE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(probe) = slo_probe {
        let report = probe();
        if let Some(items) = report["slo"].as_array() {
            out.push_str("# HELP owo_slo_achieving SLO 窗口内达标（1=达标）\n");
            out.push_str("# TYPE owo_slo_achieving gauge\n");
            out.push_str("# HELP owo_slo_budget_remaining SLO 错误预算剩余比例\n");
            out.push_str("# TYPE owo_slo_budget_remaining gauge\n");
            out.push_str("# HELP owo_slo_violations_total SLO 累计违规数\n");
            out.push_str("# TYPE owo_slo_violations_total counter\n");
            for item in items {
                // R12：label 值转义（`\`/`"`），保证 text exposition 格式对外可解析。
                let name = esc_label(item["name"].as_str().unwrap_or(""));
                let achieving = if item["achieving"].as_bool().unwrap_or(false) {
                    "1"
                } else {
                    "0"
                };
                let remaining = item["error_budget"]["remaining"].as_f64().unwrap_or(-1.0);
                out.push_str(&format!(
                    "owo_slo_achieving{{name=\"{name}\"}} {achieving}\n"
                ));
                out.push_str(&format!(
                    "owo_slo_budget_remaining{{name=\"{name}\"}} {remaining:.4}\n"
                ));
                out.push_str(&format!(
                    "owo_slo_violations_total{{name=\"{name}\"}} {}\n",
                    item["violations"].as_u64().unwrap_or(0)
                ));
            }
        }
    }
    // R10：用量指标（探针已注册时）。
    let usage_probe = USAGE_PROBE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if let Some(probe) = usage_probe {
        let summary = probe();
        if let Some(dims) = summary["dimensions"].as_array() {
            out.push_str("# HELP owo_usage_calls_total 用量维度调用数\n");
            out.push_str("# TYPE owo_usage_calls_total counter\n");
            out.push_str("# HELP owo_usage_tokens_total 用量维度 token 总数\n");
            out.push_str("# TYPE owo_usage_tokens_total counter\n");
            out.push_str("# HELP owo_usage_cost_usd_total 用量维度成本（USD）\n");
            out.push_str("# TYPE owo_usage_cost_usd_total counter\n");
            out.push_str("# HELP owo_usage_budget_spent_usd 维度预算已花费（USD）\n");
            out.push_str("# TYPE owo_usage_budget_spent_usd gauge\n");
            out.push_str("# HELP owo_usage_budget_limit_usd 维度预算上限（USD）\n");
            out.push_str("# TYPE owo_usage_budget_limit_usd gauge\n");
            for dim in dims {
                // R12：label 值转义；预算上限与花费成对输出，供花费/预算对照图。
                let name = esc_label(dim["dimension"].as_str().unwrap_or(""));
                out.push_str(&format!(
                    "owo_usage_calls_total{{dimension=\"{name}\"}} {}\n",
                    dim["calls"].as_u64().unwrap_or(0)
                ));
                out.push_str(&format!(
                    "owo_usage_tokens_total{{dimension=\"{name}\"}} {}\n",
                    dim["total_tokens"].as_u64().unwrap_or(0)
                ));
                out.push_str(&format!(
                    "owo_usage_cost_usd_total{{dimension=\"{name}\"}} {}\n",
                    dim["cost_usd"].as_f64().unwrap_or(0.0)
                ));
                if let Some(budget) = dim["budget"].as_object() {
                    out.push_str(&format!(
                        "owo_usage_budget_spent_usd{{dimension=\"{name}\"}} {}\n",
                        budget["spent_usd"].as_f64().unwrap_or(0.0)
                    ));
                    out.push_str(&format!(
                        "owo_usage_budget_limit_usd{{dimension=\"{name}\"}} {}\n",
                        budget["limit_usd"].as_f64().unwrap_or(0.0)
                    ));
                }
            }
        }
        let hard_stop = if summary["hard_stop"].as_bool().unwrap_or(false) {
            "1"
        } else {
            "0"
        };
        out.push_str("# HELP owo_usage_hard_stop 用量预算硬熔断（1=熔断）\n");
        out.push_str("# TYPE owo_usage_hard_stop gauge\n");
        out.push_str(&format!("owo_usage_hard_stop {hard_stop}\n"));
    }
    out
}

/// Prometheus label value 转义（R12）：`\` 与 `"` 反转义，保证 text exposition 格式合法。
fn esc_label(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

// ==================== R10：用量探针（prometheus 追加指标） ====================

/// 用量探针：返回 `usage::global().summary()` 同构 JSON（数据面在 usage.rs）。
pub type UsageProbe = Arc<dyn Fn() -> Value + Send + Sync>;

static USAGE_PROBE: Mutex<Option<UsageProbe>> = Mutex::new(None);

/// 注册用量探针（主控接线：`register_usage_probe(Arc::new(|| usage::global().summary()))`）。
#[allow(dead_code)] // 仅供主控接线与 observability_tests 以 #[path] 独立编译调用。
pub fn register_usage_probe(probe: UsageProbe) {
    let mut slot = USAGE_PROBE.lock().unwrap_or_else(|e| e.into_inner());
    *slot = Some(probe);
}

/// 仅供测试：清空用量探针。
#[allow(dead_code)] // 仅供 observability_tests 以 #[path] 独立编译调用；lib 目标内无引用。
pub fn reset_usage_probe_for_test() {
    let mut slot = USAGE_PROBE.lock().unwrap_or_else(|e| e.into_inner());
    *slot = None;
}

// ==================== R10：可选遥测聚合器（默认关，不含内容） ====================

/// 遥测开关（默认关）。开启后仅聚合：功能计数、错误码分布、性能分位；
/// 不含任何消息/提示词/输出/文件内容。字段说明见 `telemetry_data_dictionary()`。
static TELEMETRY_ENABLED: AtomicBool = AtomicBool::new(false);

/// 功能计数：turn / tool / approval / event / model_call 等（仅数字）。
static TELEMETRY_COUNTERS: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);

/// 错误码分布：code → 次数（无请求内容）。
static TELEMETRY_ERROR_CODES: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);

/// 设置遥测开关（主控接线：CLI/设置面板联动；默认 false）。
#[allow(dead_code)] // 仅供主控接线与 observability_tests 以 #[path] 独立编译调用。
pub fn set_telemetry_enabled(enabled: bool) {
    TELEMETRY_ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn telemetry_enabled() -> bool {
    TELEMETRY_ENABLED.load(Ordering::Relaxed)
}

/// 记录功能计数（接线方在关键路径调用；默认关时零开销）。
#[allow(dead_code)] // 仅供接线方与 observability_tests 以 #[path] 独立编译调用。
pub fn record_telemetry_counter(name: &str, delta: u64) {
    if !telemetry_enabled() {
        return;
    }
    let mut guard = TELEMETRY_COUNTERS.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    *map.entry(name.to_string()).or_default() += delta;
}

/// 记录错误码分布（接线方在错误响应时调用；默认关时零开销）。
#[allow(dead_code)] // 仅供接线方与 observability_tests 以 #[path] 独立编译调用。
pub fn record_telemetry_error(code: &str) {
    if !telemetry_enabled() {
        return;
    }
    let mut guard = TELEMETRY_ERROR_CODES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    *map.entry(code.to_string()).or_default() += 1;
}

/// 遥测数据字典（聚合字段说明，供隐私声明/面板引用）。
pub fn telemetry_data_dictionary() -> Value {
    json!({
        "enabled": "布尔：遥测开关（默认 false）",
        "counters": "功能计数：turn/tool/approval/event/model_call 等，仅数字",
        "error_codes": "错误码分布：code → 次数，无请求内容",
        "performance": "性能分位：工具调度 p50/p95（ms）",
        "note": "不含任何消息、提示词、输出或文件内容；不上报原始数据",
    })
}

/// 遥测状态（面板/诊断展示）：开关 + 聚合摘要 + 数据字典。
/// 关闭时 counters/error_codes 为空；开启后随接线方打点累积。
pub fn telemetry_status() -> Value {
    let counters = TELEMETRY_COUNTERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default();
    let codes = TELEMETRY_ERROR_CODES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default();
    let (tool_p50, tool_p95) = with_runtime(|metrics| {
        let mut tool_durations = metrics
            .tool_durations_ms
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        tool_durations.sort_unstable();
        (
            percentile(&tool_durations, 0.5),
            percentile(&tool_durations, 0.95),
        )
    });
    json!({
        "enabled": telemetry_enabled(),
        "counters": counters,
        "error_codes": codes,
        "performance": {
            "tool_p50_ms": tool_p50,
            "tool_p95_ms": tool_p95,
        },
        "data_dictionary": telemetry_data_dictionary(),
        "note": "仅聚合指标，不含消息/提示词/输出内容；默认关闭",
    })
}

/// GET /metrics/telemetry/status（R10）：遥测开关 + 聚合摘要 + 数据字典。
async fn telemetry_status_handler(State(_state): State<Arc<AppState>>) -> ApiResult {
    Ok(Json(telemetry_status()))
}

// ==================== R9：SLO 告警与周期报告探针 ====================

/// SLO 告警探针：返回 `{count, rules, alerts}`（数据面在 slo.rs）。
pub type SloAlertsProbe = Arc<dyn Fn() -> Value + Send + Sync>;

static SLO_ALERTS_PROBE: Mutex<Option<SloAlertsProbe>> = Mutex::new(None);

/// 注册 SLO 告警探针（主控接线：`register_slo_alerts_probe(Arc::new(slo::alerts_json_closure))`）。
#[allow(dead_code)] // 仅供主控接线与 observability_tests 以 #[path] 独立编译调用。
pub fn register_slo_alerts_probe(probe: SloAlertsProbe) {
    let mut slot = SLO_ALERTS_PROBE.lock().unwrap_or_else(|e| e.into_inner());
    *slot = Some(probe);
}

/// 仅供测试：清空 SLO 告警探针。
#[allow(dead_code)] // 仅供 observability_tests 以 #[path] 独立编译调用；lib 目标内无引用。
pub fn reset_slo_alerts_probe_for_test() {
    let mut slot = SLO_ALERTS_PROBE.lock().unwrap_or_else(|e| e.into_inner());
    *slot = None;
}

/// SLO 周期报告探针：`days` → 周期聚合报告 JSON（数据面在 slo.rs）。
pub type SloPeriodProbe = Arc<dyn Fn(u64) -> Value + Send + Sync>;

static SLO_PERIOD_PROBE: Mutex<Option<SloPeriodProbe>> = Mutex::new(None);

/// 注册 SLO 周期报告探针（主控接线：`register_slo_period_probe(Arc::new(slo::report_period_global))`）。
#[allow(dead_code)] // 仅供主控接线与 observability_tests 以 #[path] 独立编译调用。
pub fn register_slo_period_probe(probe: SloPeriodProbe) {
    let mut slot = SLO_PERIOD_PROBE.lock().unwrap_or_else(|e| e.into_inner());
    *slot = Some(probe);
}

/// 仅供测试：清空 SLO 周期报告探针。
#[allow(dead_code)] // 仅供 observability_tests 以 #[path] 独立编译调用；lib 目标内无引用。
pub fn reset_slo_period_probe_for_test() {
    let mut slot = SLO_PERIOD_PROBE.lock().unwrap_or_else(|e| e.into_inner());
    *slot = None;
}

#[derive(Deserialize)]
struct SloReportQuery {
    #[serde(default)]
    days: Option<u64>,
}

/// GET /metrics/slo/alerts（R9）：最近告警 + 规则（未注册探针返回空，不 panic）。
async fn slo_alerts(State(_state): State<Arc<AppState>>) -> ApiResult {
    let probe = SLO_ALERTS_PROBE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    match probe {
        Some(probe) => Ok(Json(probe())),
        None => Ok(Json(json!({
            "count": 0,
            "rules": [],
            "alerts": [],
            "note": "slo alerts probe not registered",
        }))),
    }
}

/// GET /metrics/slo/report?days=7（R9）：周期聚合报告（未注册探针返回空，不 panic）。
async fn slo_report(
    State(_state): State<Arc<AppState>>,
    Query(query): Query<SloReportQuery>,
) -> ApiResult {
    let probe = SLO_PERIOD_PROBE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let days = query.days.unwrap_or(7);
    match probe {
        Some(probe) => Ok(Json(probe(days))),
        None => Ok(Json(json!({
            "count": 0,
            "period_days": days,
            "slo": [],
            "note": "slo period probe not registered",
        }))),
    }
}

/// 路由：/metrics/*（供主控并入 build_router）。
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/metrics/overview", axum::routing::get(overview))
        .route("/metrics/turns", axum::routing::get(turns))
        .route("/metrics/tools", axum::routing::get(tools))
        .route("/metrics/health", axum::routing::get(health))
        .route("/metrics/runtime", axum::routing::get(runtime))
        .route("/metrics/slo", axum::routing::get(slo))
        .route("/metrics/slo/alerts", axum::routing::get(slo_alerts))
        .route("/metrics/slo/report", axum::routing::get(slo_report))
        .route("/metrics/prometheus", axum::routing::get(prometheus))
        .route(
            "/metrics/telemetry/status",
            axum::routing::get(telemetry_status_handler),
        )
        .with_state(state)
}
