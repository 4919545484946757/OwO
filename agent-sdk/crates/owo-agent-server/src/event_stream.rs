// R11:event_stream 质量收尾完成。
// R12:event_stream 复核完成（trace_id/背压/告警事件，无需改动）。
//! 可靠事件流（R6 Wave 1 + R7 Wave 2 + R8 trace_id + R9 告警事件）：`/events/stream` SSE 韧性契约。
//! R9:event_stream 告警发布完成，待主控接线（slo 告警监听器经 `publish_alert` 发布；
//! `publish_with_trace` 供请求头 trace 透传）。
//!
//! - 单调 `seq`：`EventStreamHub` 为每个事件分配全局单调序号（`AtomicU64`）。
//! - `Last-Event-ID` 续传：`subscribe_after(last_event_id)` 先重放历史中
//!   `seq > last_event_id` 的事件，再进入实时流 → 断线重连零丢失。
//! - 心跳：`heartbeat()` 发布可合并心跳事件；SSE 端点空闲超时发 keep-alive 注释帧。
//! - 背压：每订阅者有界队列（默认 1024）。溢出时丢弃可合并事件（进度/心跳），
//!   保留关键事件（审批/熔断/告警）：先挤掉队内最旧可合并事件，仍满则把慢消费者
//!   标记 lagged 断开——发布方永不阻塞，绝不拖垮调度器。
//! - 指标钩子（R7）：`set_metrics_observer` 注册观察者，连接开/关、发布/丢弃、
//!   慢消费者 lagged、队列深度采样均以快照样本回调（`MetricsSample`），
//!   供 observability_api 桥接（主控在 lib.rs 接线）与 soak/契约测试消费。
//! - 本模块不引用 `crate::`/`super::`，可被测试以 `#[path] mod` 独立编译；
//!   AppState 写全限定 `owo_agent_server::AppState`。

// 主控收尾接线说明：lib 目标仅引用 router/hub；其余符号由
// event_stream_tests 以 #[path] 独立编译使用，lib 内属“测试面符号”。
// 与 team_api.rs 同款模块级 allow(dead_code)（接线后保留无害）。
#![allow(dead_code)]

use axum::extract::Query;
use axum::response::sse::{Event, Sse};
use axum::response::IntoResponse;
use axum::Router;
use owo_agent_server::AppState;
use serde::Deserialize;
use serde_json::json;
use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

/// 订阅队列默认容量（事件数）。
pub const DEFAULT_QUEUE_CAPACITY: usize = 1024;
/// 历史重放窗口上限（超过则从最旧开始裁剪）。
pub const HISTORY_CAPACITY: usize = 4096;
/// SSE 空闲心跳间隔。
pub const HEARTBEAT_INTERVAL_MS: u64 = 15_000;

/// 事件类型常量：关键事件（审批/熔断/告警）与可合并事件（进度/心跳）。
pub const KIND_APPROVAL: &str = "approval";
pub const KIND_CIRCUIT: &str = "circuit";
pub const KIND_ALERT: &str = "alert";
pub const KIND_PROGRESS: &str = "progress";
pub const KIND_HEARTBEAT: &str = "heartbeat";

/// 流事件：seq 全局单调；critical 表示审批/熔断等不可丢事件。
/// R8：`trace_id` 贯穿（可选；发布方以 `publish_with_trace` 传入，SSE 帧内透传）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StreamEvent {
    pub seq: u64,
    pub kind: String,
    pub critical: bool,
    pub data: String,
    pub created_at: String,
    #[serde(default)]
    pub trace_id: Option<String>,
}

impl StreamEvent {
    /// 构造事件（seq 由 hub 分配，此处仅填 0 占位）。
    fn new(kind: &str, critical: bool, data: String, trace_id: Option<String>) -> Self {
        Self {
            seq: 0,
            kind: kind.to_string(),
            critical,
            data,
            created_at: chrono::Utc::now().to_rfc3339(),
            trace_id,
        }
    }
}

/// 订阅端：有界队列 + 状态标记（lagged=慢消费者被断开，closed=已释放）。
pub struct Subscription {
    queue: Arc<(Mutex<VecDeque<StreamEvent>>, Condvar)>,
    last_delivered: Arc<AtomicU64>,
    dropped_mergeable: Arc<AtomicU64>,
    dropped_critical: Arc<AtomicU64>,
    lagged: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    capacity: usize,
}

impl Subscription {
    /// 阻塞读取（带超时；超时返回 None，调用方可发心跳）。
    pub fn recv_blocking(&self, timeout: Duration) -> Option<StreamEvent> {
        let (lock, condvar) = &*self.queue;
        let mut queue = lock.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(event) = queue.pop_front() {
                self.last_delivered.store(event.seq, Ordering::Relaxed);
                return Some(event);
            }
            let (guard, wait_result) = condvar
                .wait_timeout(queue, timeout)
                .unwrap_or_else(|e| e.into_inner());
            queue = guard;
            if wait_result.timed_out() {
                return None;
            }
        }
    }

    /// 非阻塞读取。
    pub fn try_recv(&self) -> Option<StreamEvent> {
        let (lock, _condvar) = &*self.queue;
        let mut queue = lock.lock().unwrap_or_else(|e| e.into_inner());
        let event = queue.pop_front();
        if let Some(event) = &event {
            self.last_delivered.store(event.seq, Ordering::Relaxed);
        }
        event
    }

    /// 已交付的最新 seq（Last-Event-ID 续传依据）。
    #[allow(dead_code)] // 接线方在 SSE 断线续传时使用；当前 lib 目标与测试内无引用。
    pub fn last_delivered(&self) -> u64 {
        self.last_delivered.load(Ordering::Relaxed)
    }

    /// 慢消费者标记：队列溢出且无法丢弃可合并事件腾位时置位（随后被断开）。
    pub fn is_lagged(&self) -> bool {
        self.lagged.load(Ordering::Relaxed)
    }

    /// 丢弃统计：(可合并, 关键)。
    pub fn dropped(&self) -> (u64, u64) {
        (
            self.dropped_mergeable.load(Ordering::Relaxed),
            self.dropped_critical.load(Ordering::Relaxed),
        )
    }
}

/// 全局流统计快照。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct EventStreamStats {
    pub last_seq: u64,
    pub history_len: usize,
    pub active_connections: usize,
    pub queue_depth: usize,
    pub published_total: u64,
    pub dropped_mergeable_total: u64,
    pub dropped_critical_total: u64,
    /// 累计打开过的连接数（含已关闭）。
    pub connections_opened_total: u64,
    /// 累计被断开（lagged）的慢消费者次数。
    pub lagged_total: u64,
}

/// 指标样本（快照 + 本次增量字段；JSON 序列化后跨模块桥接）。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct MetricsSample {
    pub conn_opened: u64,
    pub conn_closed: u64,
    pub published: u64,
    pub dropped_mergeable: u64,
    pub dropped_critical: u64,
    pub lagged: u64,
    pub active_connections: usize,
    pub queue_depth: usize,
    pub last_seq: u64,
}

impl MetricsSample {
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "conn_opened": self.conn_opened,
            "conn_closed": self.conn_closed,
            "published": self.published,
            "dropped_mergeable": self.dropped_mergeable,
            "dropped_critical": self.dropped_critical,
            "lagged": self.lagged,
            "active_connections": self.active_connections,
            "queue_depth": self.queue_depth,
            "last_seq": self.last_seq,
        })
    }
}

/// 指标观察者：接收流快照样本（接线方转发到 observability_api，测试收集断言）。
pub type MetricsObserver = Box<dyn Fn(&MetricsSample) + Send + Sync>;

static METRICS_OBSERVER: Mutex<Option<MetricsObserver>> = Mutex::new(None);

/// 注册指标观察者（主控接线：转发到 `observability_api::ingest_metrics_sample`）。
pub fn set_metrics_observer(observer: MetricsObserver) {
    let mut slot = METRICS_OBSERVER.lock().unwrap_or_else(|e| e.into_inner());
    *slot = Some(observer);
}

/// 仅供测试：清空观察者。
#[allow(dead_code)] // 仅供 event_stream_tests / observability_tests 以 #[path] 独立编译使用。
pub fn reset_metrics_observer_for_test() {
    let mut slot = METRICS_OBSERVER.lock().unwrap_or_else(|e| e.into_inner());
    *slot = None;
}

/// 事件流集线器：历史（有界）+ 订阅者集合（每订阅者有界队列）。
pub struct EventStreamHub {
    next_seq: AtomicU64,
    history: Mutex<VecDeque<StreamEvent>>,
    subscribers: Mutex<Vec<Arc<Subscription>>>,
    published_total: AtomicU64,
    dropped_mergeable_total: AtomicU64,
    dropped_critical_total: AtomicU64,
    connections_opened_total: AtomicU64,
    lagged_total: AtomicU64,
}

impl Default for EventStreamHub {
    fn default() -> Self {
        Self::new()
    }
}

impl EventStreamHub {
    pub fn new() -> Self {
        Self {
            next_seq: AtomicU64::new(0),
            history: Mutex::new(VecDeque::new()),
            subscribers: Mutex::new(Vec::new()),
            published_total: AtomicU64::new(0),
            dropped_mergeable_total: AtomicU64::new(0),
            dropped_critical_total: AtomicU64::new(0),
            connections_opened_total: AtomicU64::new(0),
            lagged_total: AtomicU64::new(0),
        }
    }

    /// 当前最大已分配 seq（Last-Event-ID 起点）。
    pub fn last_seq(&self) -> u64 {
        self.next_seq.load(Ordering::Relaxed)
    }

    /// 发布事件：分配 seq → 追加历史 → 逐订阅者投递（溢出走背压策略）。
    /// 返回事件 seq。发布后发出指标样本（含队列深度采样）。
    /// R8：无 trace_id 的发布（等价 `publish_with_trace(.., None)`）。
    pub fn publish(&self, kind: &str, critical: bool, data: impl Into<String>) -> u64 {
        self.publish_with_trace(kind, critical, data, None)
    }

    /// 带 `trace_id` 发布（R8：请求头 trace 贯穿 SSE 事件帧）。
    pub fn publish_with_trace(
        &self,
        kind: &str,
        critical: bool,
        data: impl Into<String>,
        trace_id: Option<String>,
    ) -> u64 {
        let mut event = StreamEvent::new(kind, critical, data.into(), trace_id);
        event.seq = self.next_seq.fetch_add(1, Ordering::Relaxed) + 1;
        {
            let mut history = self.history.lock().unwrap_or_else(|e| e.into_inner());
            history.push_back(event.clone());
            while history.len() > HISTORY_CAPACITY {
                history.pop_front();
            }
        }
        self.published_total.fetch_add(1, Ordering::Relaxed);
        self.deliver(&event);
        self.emit_metrics(MetricsSample {
            published: 1,
            ..self.sample()
        });
        event.seq
    }

    /// 发布进度事件（可合并：背压下优先丢弃）。
    pub fn publish_progress(&self, data: impl Into<String>) -> u64 {
        self.publish(KIND_PROGRESS, false, data)
    }

    /// 发布审批事件（关键：溢出时保留）。
    pub fn publish_approval(&self, data: impl Into<String>) -> u64 {
        self.publish(KIND_APPROVAL, true, data)
    }

    /// 发布熔断事件（关键：溢出时保留）。
    pub fn publish_circuit(&self, data: impl Into<String>) -> u64 {
        self.publish(KIND_CIRCUIT, true, data)
    }

    /// 发布告警事件（关键：溢出时保留；R9：slo 告警监听器接线用，带 trace_id）。
    pub fn publish_alert(&self, data: impl Into<String>, trace_id: Option<String>) -> u64 {
        self.publish_with_trace(KIND_ALERT, true, data, trace_id)
    }

    /// 心跳：可合并、非关键事件（背压下优先被丢弃）。
    pub fn heartbeat(&self) -> u64 {
        self.publish(
            KIND_HEARTBEAT,
            false,
            json!({ "type": "heartbeat" }).to_string(),
        )
    }

    /// 订阅：返回订阅端 + 订阅时刻之前已发布事件的快照（供初始重放）。
    pub fn subscribe(&self) -> (Arc<Subscription>, Vec<StreamEvent>) {
        self.subscribe_after(0)
    }

    /// 按 `Last-Event-ID` 续传：重放 `seq > last_event_id` 的历史事件后进入实时流。
    pub fn subscribe_after(&self, last_event_id: u64) -> (Arc<Subscription>, Vec<StreamEvent>) {
        self.subscribe_with_capacity(DEFAULT_QUEUE_CAPACITY, last_event_id)
    }

    /// 带自定义队列容量的订阅（背压测试用小容量；生产默认 1024）。
    #[allow(dead_code)] // 仅供 event_stream_tests 以 #[path] 独立编译使用；lib 目标内无引用。
    pub fn subscribe_with_capacity(
        &self,
        capacity: usize,
        last_event_id: u64,
    ) -> (Arc<Subscription>, Vec<StreamEvent>) {
        let subscription = Arc::new(Subscription {
            queue: Arc::new((Mutex::new(VecDeque::new()), Condvar::new())),
            last_delivered: Arc::new(AtomicU64::new(last_event_id)),
            dropped_mergeable: Arc::new(AtomicU64::new(0)),
            dropped_critical: Arc::new(AtomicU64::new(0)),
            lagged: Arc::new(AtomicBool::new(false)),
            closed: Arc::new(AtomicBool::new(false)),
            capacity,
        });
        let replay = {
            let history = self.history.lock().unwrap_or_else(|e| e.into_inner());
            history
                .iter()
                .filter(|event| event.seq > last_event_id)
                .cloned()
                .collect()
        };
        self.subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Arc::clone(&subscription));
        self.connections_opened_total
            .fetch_add(1, Ordering::Relaxed);
        self.emit_metrics(MetricsSample {
            conn_opened: 1,
            ..self.sample()
        });
        (subscription, replay)
    }

    /// 活跃连接数（未关闭的订阅者）。
    pub fn active_connections(&self) -> usize {
        let subscribers = self.subscribers.lock().unwrap_or_else(|e| e.into_inner());
        subscribers
            .iter()
            .filter(|s| !s.closed.load(Ordering::Relaxed))
            .count()
    }

    /// 全部订阅者队列深度之和。
    pub fn total_queue_depth(&self) -> usize {
        let subscribers = self.subscribers.lock().unwrap_or_else(|e| e.into_inner());
        subscribers
            .iter()
            .map(|s| {
                let (queue, _) = &*s.queue;
                queue.lock().unwrap_or_else(|e| e.into_inner()).len()
            })
            .sum()
    }

    /// 全局统计快照。
    pub fn stats(&self) -> EventStreamStats {
        EventStreamStats {
            last_seq: self.last_seq(),
            history_len: self.history.lock().unwrap_or_else(|e| e.into_inner()).len(),
            active_connections: self.active_connections(),
            queue_depth: self.total_queue_depth(),
            published_total: self.published_total.load(Ordering::Relaxed),
            dropped_mergeable_total: self.dropped_mergeable_total.load(Ordering::Relaxed),
            dropped_critical_total: self.dropped_critical_total.load(Ordering::Relaxed),
            connections_opened_total: self.connections_opened_total.load(Ordering::Relaxed),
            lagged_total: self.lagged_total.load(Ordering::Relaxed),
        }
    }

    /// 指标样本快照（增量字段默认 0）。
    fn sample(&self) -> MetricsSample {
        MetricsSample {
            active_connections: self.active_connections(),
            queue_depth: self.total_queue_depth(),
            last_seq: self.last_seq(),
            ..MetricsSample::default()
        }
    }

    /// 发出指标样本（锁外调用；观察者缺失时零开销）。
    fn emit_metrics(&self, sample: MetricsSample) {
        let observer = METRICS_OBSERVER.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(observer) = observer.as_ref() {
            observer(&sample);
        }
    }

    /// 投递事件到每个订阅者（背压策略，不阻塞）。
    /// 指标样本在全部锁释放后统一发出（避免在持有 queue/subscribers 锁时
    /// 再进入 sample() 造成非重入 Mutex 死锁）。
    fn deliver(&self, event: &StreamEvent) {
        let (subscribers, mut prune) = {
            let subscribers = self.subscribers.lock().unwrap_or_else(|e| e.into_inner());
            (subscribers.clone(), Vec::new())
        };
        let mut dropped_mergeable_now = 0u64;
        let mut dropped_critical_now = 0u64;
        let mut lagged_now = 0u64;
        for subscription in &subscribers {
            if subscription.closed.load(Ordering::Relaxed) {
                prune.push(Arc::clone(subscription));
                continue;
            }
            if subscription.lagged.load(Ordering::Relaxed) {
                // 已被断开的慢消费者：后续事件一律记丢失，不再尝试投递。
                if event.critical {
                    subscription
                        .dropped_critical
                        .fetch_add(1, Ordering::Relaxed);
                    self.dropped_critical_total.fetch_add(1, Ordering::Relaxed);
                    dropped_critical_now += 1;
                } else {
                    subscription
                        .dropped_mergeable
                        .fetch_add(1, Ordering::Relaxed);
                    self.dropped_mergeable_total.fetch_add(1, Ordering::Relaxed);
                    dropped_mergeable_now += 1;
                }
                continue;
            }
            let (queue, condvar) = &*subscription.queue;
            let mut queue = queue.lock().unwrap_or_else(|e| e.into_inner());
            if queue.len() < subscription.capacity {
                queue.push_back(event.clone());
                condvar.notify_one();
                continue;
            }
            // 队列已满：可合并事件直接丢弃；关键事件挤掉队内最旧可合并事件。
            if !event.critical {
                subscription
                    .dropped_mergeable
                    .fetch_add(1, Ordering::Relaxed);
                self.dropped_mergeable_total.fetch_add(1, Ordering::Relaxed);
                dropped_mergeable_now += 1;
                continue;
            }
            let made_room = {
                let len_before = queue.len();
                queue.retain(|queued| queued.critical);
                len_before > queue.len()
            };
            if made_room {
                let evicted = subscription.capacity - queue.len();
                subscription
                    .dropped_mergeable
                    .fetch_add(evicted as u64, Ordering::Relaxed);
                self.dropped_mergeable_total
                    .fetch_add(evicted as u64, Ordering::Relaxed);
                queue.push_back(event.clone());
                condvar.notify_one();
                dropped_mergeable_now += evicted as u64;
            } else {
                // 关键事件也放不下 → 慢消费者断开（防拖垮调度器）。
                subscription.lagged.store(true, Ordering::Relaxed);
                self.lagged_total.fetch_add(1, Ordering::Relaxed);
                subscription
                    .dropped_critical
                    .fetch_add(1, Ordering::Relaxed);
                self.dropped_critical_total.fetch_add(1, Ordering::Relaxed);
                dropped_critical_now += 1;
                lagged_now += 1;
            }
        }
        if !prune.is_empty() {
            let mut stored = self.subscribers.lock().unwrap_or_else(|e| e.into_inner());
            stored.retain(|s| !s.closed.load(Ordering::Relaxed));
        }
        if dropped_mergeable_now + dropped_critical_now + lagged_now > 0 {
            self.emit_metrics(MetricsSample {
                dropped_mergeable: dropped_mergeable_now,
                dropped_critical: dropped_critical_now,
                lagged: lagged_now,
                ..self.sample()
            });
        }
    }

    /// 标记订阅关闭（SSE 连接结束/被断开时调用），随后从注册表回收。
    pub fn close(&self, subscription: &Arc<Subscription>) {
        subscription.closed.store(true, Ordering::Relaxed);
        {
            let mut subscribers = self.subscribers.lock().unwrap_or_else(|e| e.into_inner());
            subscribers.retain(|s| !s.closed.load(Ordering::Relaxed));
        }
        self.emit_metrics(MetricsSample {
            conn_closed: 1,
            ..self.sample()
        });
    }
}

// 事件可否被挤掉腾位：仅可合并事件（进度/心跳）允许；关键事件不可挤。

/// 全局单例集线器（SSE 路由使用；测试用 `reset_hub_for_test` 重置）。
static HUB: OnceLock<Arc<EventStreamHub>> = OnceLock::new();

pub fn hub() -> &'static Arc<EventStreamHub> {
    HUB.get_or_init(|| Arc::new(EventStreamHub::new()))
}

/// 仅供测试：重置全局单例。
#[allow(dead_code)] // 仅供 event_stream_tests 以 #[path] 独立编译使用；lib 目标内无引用。
pub fn reset_hub_for_test() {
    let _ = HUB.set(Arc::new(EventStreamHub::new()));
}

/// 事件的数据 JSON（含 seq，SSE data 负载）。R8：trace_id 有值时透传。
fn event_json(event: &StreamEvent) -> String {
    let mut frame = json!({
        "seq": event.seq,
        "kind": event.kind,
        "critical": event.critical,
        "data": event.data,
        "created_at": event.created_at,
    });
    if let Some(trace_id) = &event.trace_id {
        frame["trace_id"] = json!(trace_id);
    }
    frame.to_string()
}

/// 把事件编码为完整 SSE 帧文本（`event: <kind>\ndata: <json>\n\n`）。
#[allow(dead_code)] // 仅供 event_stream_tests 以 #[path] 独立编译使用。
pub fn sse_frame_text(event: &StreamEvent) -> String {
    format!("event: {}\ndata: {}\n\n", event.kind, event_json(event))
}

/// SSE 端点查询参数：`?last_event_id=`。
#[derive(Deserialize)]
struct StreamQuery {
    last_event_id: Option<u64>,
}

/// SSE 端点：`GET /events/stream`（供主控并入 build_router）。
/// 重放 `seq > last_event_id` 历史 → 实时流 → 空闲发心跳注释帧；
/// 慢消费者（lagged）直接断开。
async fn events_stream(
    Query(query): Query<StreamQuery>,
) -> Sse<UnboundedReceiverStream<Result<Event, Infallible>>> {
    let (subscription, replay) = hub().subscribe_after(query.last_event_id.unwrap_or(0));
    let (tx, rx) = mpsc::unbounded_channel::<Result<Event, Infallible>>();

    tokio::spawn(async move {
        for event in replay {
            if send_frame(&tx, &event).is_err() {
                hub().close(&subscription);
                return;
            }
        }
        loop {
            match subscription.recv_blocking(Duration::from_millis(HEARTBEAT_INTERVAL_MS)) {
                Some(event) => {
                    if send_frame(&tx, &event).is_err() {
                        break;
                    }
                }
                None => {
                    if tx.send(Ok(Event::default().comment("keep-alive"))).is_err() {
                        break;
                    }
                }
            }
            if subscription.is_lagged() {
                // 慢消费者：断开而非拖垮发布方。
                break;
            }
        }
        hub().close(&subscription);
    });

    Sse::new(UnboundedReceiverStream::new(rx))
}

fn send_frame(
    tx: &mpsc::UnboundedSender<Result<Event, Infallible>>,
    event: &StreamEvent,
) -> Result<(), ()> {
    tx.send(Ok(Event::default()
        .event(&event.kind)
        .data(event_json(event))))
        .map_err(|_| ())
}

/// 路由：/events/stream（供主控并入 build_router）。
pub fn router(_state: Arc<AppState>) -> Router {
    Router::new().route("/events/stream", axum::routing::get(events_stream))
}

/// SSE 健康检查辅助（供测试断言响应形态）。
#[allow(dead_code)] // 仅供 event_stream_tests 以 #[path] 独立编译使用。
pub fn sse_response_ok(response: &axum::response::Response) -> bool {
    response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("text/event-stream"))
        .unwrap_or(false)
}

// 占位：确保 IntoResponse 路径在独立编译测试中类型完整。
#[allow(dead_code)]
fn _type_probe(response: axum::response::Response) -> impl IntoResponse {
    response
}
