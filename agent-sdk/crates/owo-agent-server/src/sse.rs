//! 云端任务进度 SSE 集线器（Lane D Part 2）。
//!
//! - [`CloudSseHub`]：按 task_id 一个 `broadcast::Sender<String>` + 事件历史（订阅先重放历史再流式）。
//! - [`hub()`]：模块内 `OnceLock` 单例。
//! - [`SseHubSink`]：`owo_agent_core::cloud_exec::ProgressSink` 适配器，把
//!   `CloudProgress` 各变体序列化为 JSON 帧发布到 hub。
//! - [`router`]：`GET /cloud/tasks/{id}/events` → text/event-stream（重放历史 + 实时流）。
//!
//! 接线说明（供主控）：在 `lib.rs::cloud_task_submit` 中用 `sse::sink(task_id.clone())`
//! 作为 `run_next` 的 ProgressSink；并把 `sse::router(state)` 合并进 `build_router`。
//! 本模块不引用 `crate::`/`super::`，可被测试以 `#[path] mod` 独立编译。

use axum::extract::Path;
use axum::response::sse::{Event, Sse};
use axum::response::IntoResponse;
use axum::Router;
use owo_agent_core::cloud_exec::{CloudProgress, ProgressSink};
use owo_agent_server::AppState;
use serde_json::json;
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

/// SSE 集线器：task_id → 广播通道 + 事件历史（重放）。
pub struct CloudSseHub {
    senders: Mutex<HashMap<String, broadcast::Sender<String>>>,
    history: Mutex<HashMap<String, Vec<String>>>,
}

impl CloudSseHub {
    pub fn new() -> Self {
        Self {
            senders: Mutex::new(HashMap::new()),
            history: Mutex::new(HashMap::new()),
        }
    }

    /// 订阅：返回（广播接收端，历史事件副本）。历史先于实时流重放。
    pub fn subscribe(&self, task_id: &str) -> (broadcast::Receiver<String>, Vec<String>) {
        let sender = {
            let mut senders = self.senders.lock().unwrap_or_else(|e| e.into_inner());
            senders
                .entry(task_id.to_string())
                .or_insert_with(|| broadcast::channel(256).0)
                .clone()
        };
        let history = self
            .history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(task_id)
            .cloned()
            .unwrap_or_default();
        (sender.subscribe(), history)
    }

    /// 发布事件帧（历史追加 + 广播）。返回订阅者数。
    pub fn publish(&self, task_id: &str, payload: String) -> usize {
        {
            let mut history = self.history.lock().unwrap_or_else(|e| e.into_inner());
            let slot = history.entry(task_id.to_string()).or_default();
            slot.push(payload.clone());
            if slot.len() > 512 {
                let drop = slot.len() - 512;
                slot.drain(..drop);
            }
        }
        let sender = {
            let mut senders = self.senders.lock().unwrap_or_else(|e| e.into_inner());
            senders
                .entry(task_id.to_string())
                .or_insert_with(|| broadcast::channel(256).0)
                .clone()
        };
        sender.send(payload).unwrap_or(0)
    }

    /// 读取历史（供测试/审计）。
    #[allow(dead_code)] // 仅供 cloud_sse_tests 以 #[path] 独立编译使用；lib 目标内无引用。
    pub fn history(&self, task_id: &str) -> Vec<String> {
        self.history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(task_id)
            .cloned()
            .unwrap_or_default()
    }
}

impl Default for CloudSseHub {
    fn default() -> Self {
        Self::new()
    }
}

/// 全局单例集线器。
static HUB: OnceLock<Arc<CloudSseHub>> = OnceLock::new();

pub fn hub() -> &'static Arc<CloudSseHub> {
    HUB.get_or_init(|| Arc::new(CloudSseHub::new()))
}

/// 把 `CloudProgress` 变体序列化为 JSON 帧（event 名 + 变体字段）。
pub fn progress_frame(event: &CloudProgress) -> String {
    let (kind, payload) = match event {
        CloudProgress::Snapshotting { task_id } => ("snapshotting", json!({ "task_id": task_id })),
        CloudProgress::Submitting { task_id } => ("submitting", json!({ "task_id": task_id })),
        CloudProgress::Submitted { task_id, remote_id } => (
            "submitted",
            json!({ "task_id": task_id, "remote_id": remote_id }),
        ),
        CloudProgress::Executing { task_id } => ("executing", json!({ "task_id": task_id })),
        CloudProgress::Fetching { task_id } => ("fetching", json!({ "task_id": task_id })),
        CloudProgress::Retrying {
            task_id,
            retry_count,
        } => (
            "retrying",
            json!({ "task_id": task_id, "retry_count": retry_count }),
        ),
        CloudProgress::Succeeded {
            task_id,
            diff_count,
        } => (
            "succeeded",
            json!({ "task_id": task_id, "diff_count": diff_count }),
        ),
        CloudProgress::Failed { task_id, error } => {
            ("failed", json!({ "task_id": task_id, "error": error }))
        }
        CloudProgress::Canceled { task_id } => ("canceled", json!({ "task_id": task_id })),
    };
    let mut frame = payload;
    frame["event"] = json!(kind);
    frame["kind"] = json!(kind);
    frame.to_string()
}

/// ProgressSink 适配器：emit → hub.publish（历史 + 广播）。
#[derive(Clone)]
pub struct SseHubSink {
    task_id: String,
    hub: Arc<CloudSseHub>,
}

impl SseHubSink {
    pub fn new(task_id: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            hub: Arc::clone(hub()),
        }
    }
}

impl ProgressSink for SseHubSink {
    fn emit(&self, event: &CloudProgress) {
        self.hub.publish(&self.task_id, progress_frame(event));
    }
}

/// 为 task_id 构造 sink（主控在 cloud_task_submit 中使用）。
pub fn sink(task_id: impl Into<String>) -> SseHubSink {
    SseHubSink::new(task_id)
}

/// SSE 帧文本（与 axum Event 编码语义一致：`event: progress\ndata: <frame>\n\n`）。
/// 供测试断言帧格式；路由经 `Event::data` 编码，等价输出。
#[allow(dead_code)] // 仅供 cloud_sse_tests 以 #[path] 独立编译使用。
pub fn sse_frame_text(frame: &str) -> String {
    format!("event: progress\ndata: {frame}\n\n")
}

/// SSE 事件流端点：`GET /cloud/tasks/{id}/events`。
async fn cloud_task_events(
    Path(task_id): Path<String>,
) -> Sse<UnboundedReceiverStream<Result<Event, Infallible>>> {
    let (receiver, history) = hub().subscribe(&task_id);
    let (tx, rx) = mpsc::unbounded_channel::<Result<Event, Infallible>>();

    tokio::spawn(async move {
        // 1) 重放历史（订阅前已完成的事件）。
        for frame in history {
            if tx
                .send(Ok(Event::default().event("progress").data(frame)))
                .is_err()
            {
                return;
            }
        }
        // 2) 实时流。
        let mut receiver = receiver;
        loop {
            match receiver.recv().await {
                Ok(frame) => {
                    if tx
                        .send(Ok(Event::default().event("progress").data(frame)))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    let _ = skipped;
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    });

    Sse::new(UnboundedReceiverStream::new(rx))
}

/// Lane D Part 2 路由：/cloud/tasks/{id}/events（供主控并入 build_router）。
pub fn router(_state: Arc<AppState>) -> Router {
    Router::new().route(
        "/cloud/tasks/{id}/events",
        axum::routing::get(cloud_task_events),
    )
}

/// 供依赖方测试/调试：把 hub 重置为全新实例（仅测试进程内调用）。
#[allow(dead_code)] // 仅供 cloud_sse_tests 以 #[path] 独立编译使用。
pub fn reset_hub_for_test() {
    let _ = HUB.set(Arc::new(CloudSseHub::new()));
}

/// 健康检查辅助（供测试断言响应形态）。
#[allow(dead_code)] // 仅供 cloud_sse_tests 以 #[path] 独立编译使用。
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
