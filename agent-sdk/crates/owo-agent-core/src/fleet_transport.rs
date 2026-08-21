// R12:fleet_transport 完成，待主控接线
//! 控制面传输抽象：任务经 transport 提交（submit/status/cancel/event）。
//!
//! 设计来源：《多Agent并行体系-生产级设计与跨机扩展-2026-08-16.md》§4 传输层：
//! - [`FleetTransport`]：节点间任务提交/查询/取消/事件订阅的统一接口。
//! - [`InMemoryTransport`]：进程内实现（单机/测试；失败注入能力）。
//! - [`LocalHttpTransport`]：本机回环 HTTP 占位（无认证；后续由 server 接线）。
//! - [`TransportWorker`]：把传输提交包装为 `goal::Worker`，任务经 transport 执行；
//!   失败/恢复语义沿用总线持久化（调用方以 `bus_store` 幂等重放兜底）。
//! - 事件统一带 `correlation_id` 与血缘（`lineage`），与 `bus_store` 持久化对齐。

use crate::goal::Worker;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 传输层任务。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportTask {
    pub task_id: String,
    /// 目标节点/worker（CapabilityCard 注册名）。
    pub worker: String,
    pub input: serde_json::Value,
    pub correlation_id: String,
    /// 血缘：父 step/产物引用（与 CAS 哈希贯通）。
    #[serde(default)]
    pub lineage: Vec<String>,
    /// 是否需审批（远程 step；提交后进入 AwaitingApproval，审批通过才执行）。
    #[serde(default)]
    pub approval_required: bool,
}

impl TransportTask {
    pub fn new(
        task_id: impl Into<String>,
        worker: impl Into<String>,
        correlation_id: impl Into<String>,
        input: serde_json::Value,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            worker: worker.into(),
            input,
            correlation_id: correlation_id.into(),
            lineage: Vec::new(),
            approval_required: false,
        }
    }
}

/// 传输层任务状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    /// 需审批（远程 step）。
    AwaitingApproval,
}

/// 传输层事件种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportEventKind {
    Progress,
    Result,
    /// 审批请求（远程 step 回传所有者设备）。
    ApprovalRequested,
    ApprovalGranted,
    Cancelled,
}

/// 传输层事件（与 bus_store 落盘格式对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportEvent {
    pub task_id: String,
    pub kind: TransportEventKind,
    pub correlation_id: String,
    #[serde(default)]
    pub payload: serde_json::Value,
    #[serde(default)]
    pub lineage: Vec<String>,
}

/// 控制面传输抽象。
#[async_trait]
pub trait FleetTransport: Send + Sync {
    fn name(&self) -> &str;
    async fn submit(&self, task: TransportTask) -> Result<(), String>;
    async fn status(&self, task_id: &str) -> Result<TransportStatus, String>;
    async fn cancel(&self, task_id: &str) -> Result<(), String>;
    /// 拉取任务事件（含审批回传）。
    async fn events(&self, task_id: &str) -> Result<Vec<TransportEvent>, String>;
    /// 审批放行：`AwaitingApproval` → 执行。不支持的传输默认拒绝（不静默降级）。
    async fn approve(&self, _task_id: &str, _approved_by: &str) -> Result<(), String> {
        Err(format!("{} 不支持审批放行", self.name()))
    }
}

// ---------- InMemoryTransport ----------

#[derive(Default, Debug)]
struct InMemoryInner {
    tasks: HashMap<String, InMemoryTask>,
}

#[derive(Debug)]
struct InMemoryTask {
    task: TransportTask,
    status: TransportStatus,
    events: Vec<TransportEvent>,
    submitted_at: Option<std::time::Instant>,
}

impl Default for InMemoryTask {
    fn default() -> Self {
        Self {
            task: TransportTask {
                task_id: String::new(),
                worker: String::new(),
                input: serde_json::Value::Null,
                correlation_id: String::new(),
                lineage: Vec::new(),
                approval_required: false,
            },
            status: TransportStatus::Pending,
            events: Vec::new(),
            submitted_at: None,
        }
    }
}

/// 进程内传输（单机/测试；支持失败注入与任务 TTL 超时迁移）。
#[derive(Clone, Default, Debug)]
pub struct InMemoryTransport {
    inner: Arc<Mutex<InMemoryInner>>,
    /// 任务 TTL：超过后未完成的任务惰性迁移为 Failed（超时迁移，防孤儿/防挂起）。
    task_ttl: Option<Duration>,
}

impl InMemoryTransport {
    pub fn new() -> Self {
        Self::default()
    }

    /// 带任务 TTL 的传输：未在 TTL 内完成的任务惰性迁移为 Failed。
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(InMemoryInner::default())),
            task_ttl: Some(ttl),
        }
    }

    pub fn task_ttl(&self) -> Option<Duration> {
        self.task_ttl
    }

    /// 任务总数。
    pub fn task_count(&self) -> usize {
        self.inner.lock().map(|i| i.tasks.len()).unwrap_or(0)
    }

    /// 任务事件（供断言/重放）。
    pub fn task_events(&self, task_id: &str) -> Vec<TransportEvent> {
        self.inner
            .lock()
            .ok()
            .and_then(|i| i.tasks.get(task_id).map(|t| t.events.clone()))
            .unwrap_or_default()
    }

    /// 全部任务 id（枚举；供执行器/观测）。
    pub fn task_ids(&self) -> Vec<String> {
        self.inner
            .lock()
            .map(|i| i.tasks.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// 任务定义（原始提交信息；供查询/执行器）。
    pub fn task(&self, task_id: &str) -> Option<TransportTask> {
        self.inner
            .lock()
            .ok()
            .and_then(|i| i.tasks.get(task_id).map(|t| t.task.clone()))
    }

    /// 任务状态。
    pub fn task_status(&self, task_id: &str) -> Option<TransportStatus> {
        self.inner
            .lock()
            .ok()
            .and_then(|i| i.tasks.get(task_id).map(|t| t.status))
    }

    /// 惰性超时迁移：超过 TTL 且未完成的任务 → Failed（超时迁移语义）。
    fn migrate_expired(&self, inner: &mut InMemoryInner) {
        let Some(ttl) = self.task_ttl else {
            return;
        };
        let now = std::time::Instant::now();
        for t in inner.tasks.values_mut() {
            if matches!(
                t.status,
                TransportStatus::Pending
                    | TransportStatus::Running
                    | TransportStatus::AwaitingApproval
            ) {
                if let Some(sub) = t.submitted_at {
                    if now.duration_since(sub) >= ttl {
                        t.status = TransportStatus::Failed;
                        t.events.push(TransportEvent {
                            task_id: t.task.task_id.clone(),
                            kind: TransportEventKind::Cancelled,
                            correlation_id: t.task.correlation_id.clone(),
                            payload: serde_json::json!("任务超时迁移（无完成）"),
                            lineage: t.task.lineage.clone(),
                        });
                    }
                }
            }
        }
    }

    /// 完成一个任务（模拟远端执行产出；事件挂到任务上）。
    /// 幂等：已终态（Succeeded/Failed/Cancelled）的任务不再完成（防重复执行/重复事件）。
    pub fn complete_task(&self, task_id: &str, ok: bool, payload: serde_json::Value) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let Some(t) = inner.tasks.get_mut(task_id) else {
            return false;
        };
        if matches!(
            t.status,
            TransportStatus::Succeeded | TransportStatus::Failed | TransportStatus::Cancelled
        ) {
            return false;
        }
        let kind = if ok {
            TransportEventKind::Result
        } else {
            TransportEventKind::Cancelled
        };
        let event = TransportEvent {
            task_id: task_id.to_string(),
            kind,
            correlation_id: t.task.correlation_id.clone(),
            payload,
            lineage: t.task.lineage.clone(),
        };
        t.events.push(event);
        t.status = if ok {
            TransportStatus::Succeeded
        } else {
            TransportStatus::Failed
        };
        true
    }

    /// 审批通过：`AwaitingApproval` → `Running` + `ApprovalGranted` 事件（远程审批回传闭环）。
    pub fn approve_task(&self, task_id: &str, approved_by: &str) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let Some(t) = inner.tasks.get_mut(task_id) else {
            return false;
        };
        if t.status != TransportStatus::AwaitingApproval {
            return false;
        }
        t.events.push(TransportEvent {
            task_id: task_id.to_string(),
            kind: TransportEventKind::ApprovalGranted,
            correlation_id: t.task.correlation_id.clone(),
            payload: serde_json::json!({ "approved_by": approved_by }),
            lineage: t.task.lineage.clone(),
        });
        t.status = TransportStatus::Running;
        true
    }

    /// 审批拒绝：`AwaitingApproval` → `Cancelled` + 事件。
    pub fn deny_task(&self, task_id: &str, reason: &str) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let Some(t) = inner.tasks.get_mut(task_id) else {
            return false;
        };
        if t.status != TransportStatus::AwaitingApproval {
            return false;
        }
        t.events.push(TransportEvent {
            task_id: task_id.to_string(),
            kind: TransportEventKind::Cancelled,
            correlation_id: t.task.correlation_id.clone(),
            payload: serde_json::json!({ "reason": reason }),
            lineage: t.task.lineage.clone(),
        });
        t.status = TransportStatus::Cancelled;
        true
    }
}

#[async_trait]
impl FleetTransport for InMemoryTransport {
    fn name(&self) -> &str {
        "in-memory"
    }

    async fn submit(&self, task: TransportTask) -> Result<(), String> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "内存传输锁异常".to_string())?;
        if inner.tasks.contains_key(&task.task_id) {
            return Err(format!("任务重复提交：{}", task.task_id));
        }
        let (status, mut events) = if task.approval_required {
            // 审批任务：进入 AwaitingApproval，回传审批事件（payload 约定 approval.owner_device/summary）。
            let owner_device = task
                .input
                .pointer("/approval/owner_device")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let summary = task
                .input
                .pointer("/approval/summary")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let ev = TransportEvent {
                task_id: task.task_id.clone(),
                kind: TransportEventKind::ApprovalRequested,
                correlation_id: task.correlation_id.clone(),
                payload: serde_json::json!({ "owner_device": owner_device, "summary": summary }),
                lineage: task.lineage.clone(),
            };
            (TransportStatus::AwaitingApproval, vec![ev])
        } else {
            (TransportStatus::Running, Vec::new())
        };
        events.dedup();
        inner.tasks.insert(
            task.task_id.clone(),
            InMemoryTask {
                task,
                status,
                events,
                submitted_at: Some(std::time::Instant::now()),
            },
        );
        Ok(())
    }

    async fn status(&self, task_id: &str) -> Result<TransportStatus, String> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "内存传输锁异常".to_string())?;
        self.migrate_expired(&mut inner);
        inner
            .tasks
            .get(task_id)
            .map(|t| t.status)
            .ok_or_else(|| format!("未知任务：{task_id}"))
    }

    async fn cancel(&self, task_id: &str) -> Result<(), String> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "内存传输锁异常".to_string())?;
        let Some(t) = inner.tasks.get_mut(task_id) else {
            return Err(format!("未知任务：{task_id}"));
        };
        if matches!(
            t.status,
            TransportStatus::Succeeded | TransportStatus::Failed | TransportStatus::Cancelled
        ) {
            return Ok(());
        }
        t.events.push(TransportEvent {
            task_id: task_id.to_string(),
            kind: TransportEventKind::Cancelled,
            correlation_id: t.task.correlation_id.clone(),
            payload: serde_json::Value::Null,
            lineage: t.task.lineage.clone(),
        });
        t.status = TransportStatus::Cancelled;
        Ok(())
    }

    async fn events(&self, task_id: &str) -> Result<Vec<TransportEvent>, String> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "内存传输锁异常".to_string())?;
        self.migrate_expired(&mut inner);
        let Some(t) = inner.tasks.get(task_id) else {
            return Err(format!("未知任务：{task_id}"));
        };
        Ok(t.events.clone())
    }

    async fn approve(&self, task_id: &str, approved_by: &str) -> Result<(), String> {
        if self.approve_task(task_id, approved_by) {
            Ok(())
        } else {
            Err(format!("任务 {task_id} 不在审批等待态，无法放行"))
        }
    }
}

// ---------- HttpTransport：控制面 ↔ 节点 agent HTTP 契约 ----------

/// HTTP 传输：控制面（server `/fleet/*`）与节点 agent 的真实回环/局域网契约。
///
/// - `submit`  → `POST /fleet/tasks/submit`，`Idempotency-Key` 头 = task_id（幂等键）。
/// - `status`  → `GET  /fleet/tasks/{id}`
/// - `cancel`  → `POST /fleet/tasks/{id}/cancel`
/// - `events`  → `GET  /fleet/tasks/{id}/events?format=json`（一次性 JSON 拉取）
/// - `approve` → `POST /fleet/approvals/{id}/respond`（审批放行）
///
/// 每请求超时 + 有限重试（指数退避）：4xx 确定性错误不重试，网络/5xx 重试。
/// 未配置 base_url 时显式不可用（不静默降级）。
#[derive(Debug, Clone)]
pub struct HttpTransport {
    base_url: String,
    client: reqwest::Client,
    /// 单请求超时（默认 5s）。
    timeout: Duration,
    /// 失败重试上限（默认 3；不包含首次）。
    max_retries: u32,
}

impl HttpTransport {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_config(base_url, Duration::from_secs(5), 3)
    }

    pub fn with_config(base_url: impl Into<String>, timeout: Duration, max_retries: u32) -> Self {
        Self {
            base_url: base_url.into(),
            client: reqwest::Client::new(),
            timeout,
            max_retries,
        }
    }

    pub fn is_configured(&self) -> bool {
        !self.base_url.is_empty()
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn max_retries(&self) -> u32 {
        self.max_retries
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), path)
    }

    /// 发请求 + 有限重试：4xx 不重试；网络错误/5xx 指数退避重试。
    async fn send_with_retry(
        &self,
        method: &str,
        url: String,
        body: Option<serde_json::Value>,
        idempotency_key: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let mut attempt = 0u32;
        loop {
            let mut req = match method {
                "GET" => self.client.get(&url),
                "POST" => self.client.post(&url),
                other => return Err(format!("HttpTransport 不支持方法 {other}")),
            };
            if let Some(key) = idempotency_key {
                req = req.header("Idempotency-Key", key);
            }
            if let Some(body) = &body {
                req = req.json(body);
            }
            let outcome = req.timeout(self.timeout).send().await;
            match outcome {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        return resp.json::<serde_json::Value>().await.map_err(|e| {
                            format!("HttpTransport 响应解析失败（{status} {url}）：{e}")
                        });
                    }
                    let text = resp.text().await.unwrap_or_default();
                    if status.is_client_error() {
                        return Err(format!("HttpTransport {status} {url}（{method}）：{text}"));
                    }
                    // 5xx：可重试。
                    tracing::warn!("HttpTransport {status} {url}（{method}）：{text}");
                }
                Err(e) => {
                    // 网络/超时：可重试。
                    tracing::warn!("HttpTransport {method} {url} 失败：{e}");
                }
            }
            attempt += 1;
            if attempt >= self.max_retries {
                return Err(format!(
                    "HttpTransport {method} {url} 重试 {attempt} 次仍失败"
                ));
            }
            tokio::time::sleep(Duration::from_millis(50 * (1u64 << attempt.min(6)))).await;
        }
    }
}

#[async_trait]
impl FleetTransport for HttpTransport {
    fn name(&self) -> &str {
        "http"
    }

    async fn submit(&self, task: TransportTask) -> Result<(), String> {
        if !self.is_configured() {
            return Err("HttpTransport 未配置 base_url（不可用，不静默降级）".to_string());
        }
        let body = serde_json::to_value(&task).map_err(|e| format!("任务序列化失败：{e}"))?;
        self.send_with_retry(
            "POST",
            self.endpoint("/fleet/tasks/submit"),
            Some(body),
            Some(&task.task_id),
        )
        .await?;
        Ok(())
    }

    async fn status(&self, task_id: &str) -> Result<TransportStatus, String> {
        if !self.is_configured() {
            return Err("HttpTransport 未配置 base_url（不可用，不静默降级）".to_string());
        }
        let value = self
            .send_with_retry(
                "GET",
                self.endpoint(&format!("/fleet/tasks/{task_id}")),
                None,
                None,
            )
            .await?;
        let status = value
            .get("status")
            .and_then(|s| s.as_str())
            .ok_or_else(|| "fleet 响应缺 status".to_string())?;
        serde_json::from_value(serde_json::json!(status))
            .map_err(|e| format!("fleet 状态解析失败：{e}"))
    }

    async fn cancel(&self, task_id: &str) -> Result<(), String> {
        if !self.is_configured() {
            return Err("HttpTransport 未配置 base_url（不可用，不静默降级）".to_string());
        }
        self.send_with_retry(
            "POST",
            self.endpoint(&format!("/fleet/tasks/{task_id}/cancel")),
            None,
            None,
        )
        .await?;
        Ok(())
    }

    async fn events(&self, task_id: &str) -> Result<Vec<TransportEvent>, String> {
        if !self.is_configured() {
            return Err("HttpTransport 未配置 base_url（不可用，不静默降级）".to_string());
        }
        let value = self
            .send_with_retry(
                "GET",
                self.endpoint(&format!("/fleet/tasks/{task_id}/events?format=json")),
                None,
                None,
            )
            .await?;
        serde_json::from_value(value).map_err(|e| format!("fleet 事件解析失败：{e}"))
    }

    async fn approve(&self, task_id: &str, approved_by: &str) -> Result<(), String> {
        if !self.is_configured() {
            return Err("HttpTransport 未配置 base_url（不可用，不静默降级）".to_string());
        }
        self.send_with_retry(
            "POST",
            self.endpoint(&format!("/fleet/approvals/{task_id}/respond")),
            Some(serde_json::json!({
                "decision": "approve",
                "approved_by": approved_by,
            })),
            None,
        )
        .await?;
        Ok(())
    }
}

// ---------- TransportWorker：任务经 transport 提交 ----------

/// 把传输提交包装为 `goal::Worker`（任务经 transport 执行；
/// 失败/恢复语义沿用总线持久化：调用方以 BusStore 幂等重放兜底）。
/// 默认带等待超时：超时即 cancel 任务并返回错误（防远端任务孤儿/挂起）。
#[derive(Clone)]
pub struct TransportWorker {
    transport: Arc<dyn FleetTransport>,
    worker: String,
    /// 等待终态超时（None = 不超时，仅受调用方预算兜底）。
    timeout: Option<Duration>,
}

/// TransportWorker 默认等待超时。
pub const DEFAULT_TRANSPORT_WORKER_TIMEOUT: Duration = Duration::from_secs(60);

impl std::fmt::Debug for TransportWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransportWorker")
            .field("transport", &self.transport.name())
            .field("worker", &self.worker)
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl TransportWorker {
    pub fn new(transport: Arc<dyn FleetTransport>, worker: impl Into<String>) -> Self {
        Self {
            transport,
            worker: worker.into(),
            timeout: Some(DEFAULT_TRANSPORT_WORKER_TIMEOUT),
        }
    }

    /// 自定义等待超时（`None` = 不超时）。
    pub fn with_timeout(
        transport: Arc<dyn FleetTransport>,
        worker: impl Into<String>,
        timeout: Option<Duration>,
    ) -> Self {
        Self {
            transport,
            worker: worker.into(),
            timeout,
        }
    }

    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
    }
}

#[async_trait]
impl Worker for TransportWorker {
    fn name(&self) -> &str {
        &self.worker
    }

    async fn run(&self, input: &serde_json::Value) -> Result<String, String> {
        let task_id = format!("t-{}", uuid::Uuid::new_v4());
        let task = TransportTask::new(
            task_id.clone(),
            self.worker.clone(),
            format!("x-{}", uuid::Uuid::new_v4()),
            input.clone(),
        );
        self.transport.submit(task).await?;
        let deadline = self.timeout.map(|d| tokio::time::Instant::now() + d);
        // 轮询状态 + 拉取事件，直到终态；超时先 cancel 再报错（防孤儿/挂起）。
        loop {
            if let Some(deadline) = deadline {
                if tokio::time::Instant::now() >= deadline {
                    let _ = self.transport.cancel(&task_id).await;
                    return Err(format!("transport 任务 {task_id} 等待超时"));
                }
            }
            let status = self.transport.status(&task_id).await?;
            match status {
                TransportStatus::Succeeded => {
                    let events = self.transport.events(&task_id).await?;
                    let output = events
                        .iter()
                        .find(|e| e.kind == TransportEventKind::Result)
                        .and_then(|e| e.payload.as_str().map(|s| s.to_string()))
                        .unwrap_or_default();
                    return Ok(output);
                }
                TransportStatus::Failed | TransportStatus::Cancelled => {
                    return Err(format!("transport 任务 {task_id} 失败/取消"));
                }
                TransportStatus::AwaitingApproval => {
                    return Err(format!("transport 任务 {task_id} 等待审批"));
                }
                _ => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
    }
}
