// R12:fleet 完成，待主控接线
//! 多 Agent 并行编排内核（P0）：本地 Agent 总线、监督树与并行原语。
//!
//! 设计来源：《多Agent并行体系-生产级设计与跨机扩展-2026-08-16.md》。
//! 范围：L0 单机进程内。消息语义对齐 A2A 任务/消息子集；可靠性机制对齐 OTP 监督树；
//! 并行分解对齐多 GPU 数据并行（fan-out + 聚合）。约束：并行拓扑必须有唯一调度主；
//! 共享状态写单主或 CRDT；任何消息必须带 `correlation_id` 贯通父子。

use crate::bus_store::BusStore;
use crate::capability::CapabilityMatch;
use crate::experience_store::{Attribution, ExperienceStore, Outcome};
use crate::fleet_transport::{FleetTransport, TransportTask};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::task::JoinSet;

pub type AgentId = String;
pub type CorrelationId = String;

/// 消息种类（A2A 语义子集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageKind {
    Task,
    Result,
    Review,
    Refusal,
    Progress,
}

/// 可合并事件（进度类）允许在背压时静默丢弃。
pub fn is_mergeable(kind: MessageKind) -> bool {
    matches!(kind, MessageKind::Progress)
}

/// 总线消息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusMessage {
    pub id: u64,
    pub from: AgentId,
    pub to: AgentId,
    pub kind: MessageKind,
    pub correlation_id: CorrelationId,
    pub payload: serde_json::Value,
}

/// worker 生命周期事件种类（worker_pool 崩溃/重启/熔断等进入总线与审计）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerEventKind {
    Started,
    Crashed,
    Restarted,
    Fused,
    Stopped,
    BudgetAborted,
    Cancelled,
}

impl WorkerEventKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Crashed => "crashed",
            Self::Restarted => "restarted",
            Self::Fused => "fused",
            Self::Stopped => "stopped",
            Self::BudgetAborted => "budget_aborted",
            Self::Cancelled => "cancelled",
        }
    }
}

/// worker 生命周期事件（总线载荷 / 审计条目的统一结构）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerEvent {
    pub worker: AgentId,
    pub kind: WorkerEventKind,
    pub detail: String,
    pub correlation_id: CorrelationId,
}

impl WorkerEvent {
    pub fn new(
        worker: impl Into<AgentId>,
        kind: WorkerEventKind,
        detail: impl Into<String>,
        correlation_id: impl Into<CorrelationId>,
    ) -> Self {
        Self {
            worker: worker.into(),
            kind,
            detail: detail.into(),
            correlation_id: correlation_id.into(),
        }
    }
}

/// 邮箱溢出策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowPolicy {
    /// 可合并事件（进度类）丢弃，关键事件（任务/结果/评审/拒绝）保留并报满。
    DropMergeable,
    /// 全部拒绝，调用方应退避或熔断。
    Reject,
}

/// 入队结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    Pushed,
    /// 按策略丢弃（未投递）。
    Dropped,
}

/// 总线/邮箱错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusError {
    MailboxFull(usize),
    UnknownAgent(AgentId),
    /// 关键消息持久化失败（必须显式报错，不得静默丢消息）。
    Persist(String),
}

impl fmt::Display for BusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BusError::MailboxFull(cap) => write!(f, "mailbox full (capacity {cap})"),
            BusError::UnknownAgent(id) => write!(f, "unknown agent `{id}`"),
            BusError::Persist(reason) => write!(f, "总线消息持久化失败：{reason}"),
        }
    }
}

impl Error for BusError {}

/// 有界邮箱：背压语义落在 [`Mailbox::push`] 的返回上。
#[derive(Debug, Clone)]
pub struct Mailbox {
    capacity: usize,
    queue: VecDeque<BusMessage>,
}

impl Mailbox {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            queue: VecDeque::new(),
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub fn push(
        &mut self,
        msg: BusMessage,
        policy: OverflowPolicy,
    ) -> Result<PushOutcome, BusError> {
        if self.queue.len() < self.capacity {
            self.queue.push_back(msg);
            return Ok(PushOutcome::Pushed);
        }
        match policy {
            OverflowPolicy::DropMergeable if is_mergeable(msg.kind) => Ok(PushOutcome::Dropped),
            _ => Err(BusError::MailboxFull(self.capacity)),
        }
    }

    pub fn drain(&mut self) -> Vec<BusMessage> {
        self.queue.drain(..).collect()
    }
}

/// 本地 Agent 总线：注册表 + 定向/广播投递 + 可选持久化（断点重放）。
#[derive(Clone, Default, Debug)]
pub struct AgentBus {
    mailboxes: Arc<Mutex<HashMap<AgentId, Mailbox>>>,
    next_id: Arc<AtomicU64>,
    /// 可选总线持久化存储（关键消息落盘；`replay_store` 断点重放）。
    store: Arc<Mutex<Option<BusStore>>>,
}

impl AgentBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// 挂接总线持久化存储：此后 send 的关键消息（Task/Result/Review/Refusal）自动落盘。
    pub async fn attach_store(&self, store: BusStore) {
        *self.store.lock().await = Some(store);
    }

    /// 断点重放：把已持久化的关键消息按序重新投递到已注册 agent 的邮箱
    /// （接收方以 `dedupe_messages` 幂等去重，保证不重复执行）。返回成功投递数。
    pub async fn replay_store(&self) -> usize {
        let Some(store) = self.store.lock().await.clone() else {
            return 0;
        };
        let msgs = store.replay_messages();
        let mut delivered = 0;
        let mut boxes = self.mailboxes.lock().await;
        for msg in msgs {
            if let Some(mailbox) = boxes.get_mut(&msg.to) {
                if matches!(
                    mailbox.push(msg, OverflowPolicy::Reject),
                    Ok(PushOutcome::Pushed)
                ) {
                    delivered += 1;
                }
            }
        }
        delivered
    }

    /// 已持久化消息数（诊断用）。
    pub async fn store_len(&self) -> usize {
        self.store
            .lock()
            .await
            .as_ref()
            .map(|s| s.len())
            .unwrap_or(0)
    }

    pub async fn register(&self, id: impl Into<AgentId>, capacity: usize) {
        let mut boxes = self.mailboxes.lock().await;
        boxes.insert(id.into(), Mailbox::new(capacity));
    }

    pub async fn unregister(&self, id: &str) -> bool {
        self.mailboxes.lock().await.remove(id).is_some()
    }

    pub async fn contains(&self, id: &str) -> bool {
        self.mailboxes.lock().await.contains_key(id)
    }

    pub async fn agent_count(&self) -> usize {
        self.mailboxes.lock().await.len()
    }

    pub async fn send(
        &self,
        from: impl Into<AgentId>,
        to: impl Into<AgentId>,
        kind: MessageKind,
        correlation_id: impl Into<CorrelationId>,
        payload: serde_json::Value,
        policy: OverflowPolicy,
    ) -> Result<u64, BusError> {
        let from = from.into();
        let to = to.into();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = BusMessage {
            id,
            from,
            to: to.clone(),
            kind,
            correlation_id: correlation_id.into(),
            payload,
        };
        // 关键消息先持久化（崩溃后可重放；幂等去重由 BusStore 保证），进度类按策略。
        if let Some(store) = self.store.lock().await.clone() {
            if store.should_persist(msg.kind) {
                store.persist(&msg).map_err(BusError::Persist)?;
            }
        }
        let mut boxes = self.mailboxes.lock().await;
        let mailbox = boxes
            .get_mut(&to)
            .ok_or_else(|| BusError::UnknownAgent(to.clone()))?;
        mailbox.push(msg, policy)?;
        Ok(id)
    }

    /// 广播到所有已注册 agent，返回成功投递的 agent 列表（溢出/未注册者跳过）。
    /// 持久化：同一逻辑消息（correlation_id+种类+载荷相同）只落盘一次（幂等去重）。
    pub async fn broadcast(
        &self,
        from: impl Into<AgentId>,
        topic: impl Into<AgentId>,
        kind: MessageKind,
        correlation_id: impl Into<CorrelationId>,
        payload: serde_json::Value,
        policy: OverflowPolicy,
    ) -> Vec<AgentId> {
        let from = from.into();
        let topic = topic.into();
        let correlation_id = correlation_id.into();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut boxes = self.mailboxes.lock().await;
        let ids: Vec<AgentId> = boxes.keys().cloned().collect();
        let mut delivered = Vec::with_capacity(ids.len());
        for to in ids {
            let msg = BusMessage {
                id,
                from: from.clone(),
                to: to.clone(),
                kind,
                correlation_id: correlation_id.clone(),
                payload: payload.clone(),
            };
            if let Some(mailbox) = boxes.get_mut(&to) {
                if matches!(mailbox.push(msg, policy), Ok(PushOutcome::Pushed)) {
                    delivered.push(to);
                }
            }
        }
        let _ = topic;
        // 持久化放 mailbox 锁外（仅一次；dedup_key 相同则后续调用幂等跳过）。
        if let Some(store) = self.store.lock().await.clone() {
            if store.should_persist(kind) && !delivered.is_empty() {
                let representative = BusMessage {
                    id,
                    from,
                    to: delivered[0].clone(),
                    kind,
                    correlation_id,
                    payload,
                };
                let _ = store.persist(&representative);
            }
        }
        delivered
    }

    /// 取出某个 agent 的全部待处理消息（拉模型，配合 worker 循环轮询）。
    pub async fn drain(&self, id: &str) -> Vec<BusMessage> {
        self.mailboxes
            .lock()
            .await
            .get_mut(id)
            .map(|m| m.drain())
            .unwrap_or_default()
    }

    /// 发送 worker 生命周期事件（崩溃/重启/熔断等；关键语义，溢出时拒绝而非丢弃）。
    /// `to` 为监督者 agent；载荷统一为 `WorkerEvent` JSON。
    pub async fn send_worker_event(
        &self,
        from: impl Into<AgentId>,
        to: impl Into<AgentId>,
        event: &WorkerEvent,
    ) -> Result<u64, BusError> {
        // WorkerEvent 全部字段可序列化，to_value 不会失败。
        let payload = serde_json::to_value(event).expect("WorkerEvent 序列化不可失败");
        self.send(
            from,
            to,
            MessageKind::Task,
            event.correlation_id.clone(),
            payload,
            OverflowPolicy::Reject,
        )
        .await
    }

    pub async fn pending(&self, id: &str) -> usize {
        self.mailboxes
            .lock()
            .await
            .get(id)
            .map(|m| m.len())
            .unwrap_or(0)
    }
}

/// 任务预算（对齐设计文档 2.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    pub max_turns: u32,
    pub max_steps: u32,
    pub max_duration_secs: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_turns: 50,
            max_steps: 1000,
            max_duration_secs: 600,
        }
    }
}

impl Budget {
    pub fn exceeded(&self, turns: u32, steps: u32, elapsed: Duration) -> bool {
        turns >= self.max_turns
            || steps >= self.max_steps
            || elapsed.as_secs() >= self.max_duration_secs
    }
}

/// 生成关联 ID（贯通父子 trace）。
pub fn new_correlation_id() -> CorrelationId {
    uuid::Uuid::new_v4().to_string()
}

/// 监督重启策略（OTP 映射）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RestartPolicy {
    OneForOne,
    RestForOne,
    OneForAll,
}

/// 监督规则：崩溃计数、退避基数与重启策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestartRule {
    pub max_restarts: u32,
    pub base_backoff_secs: u64,
    pub policy: RestartPolicy,
}

impl Default for RestartRule {
    fn default() -> Self {
        Self {
            max_restarts: 3,
            base_backoff_secs: 1,
            policy: RestartPolicy::OneForOne,
        }
    }
}

/// 指数退避（对齐 `cloud_exec::backoff_delay`：base·2^n，封顶 60s）。
pub fn backoff_secs(base_secs: u64, attempts: u32) -> u64 {
    base_secs.saturating_mul(1u64 << attempts.min(6)).min(60)
}

/// 监督状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisionState {
    Healthy,
    Restarting { attempts: u32, next_retry_secs: u64 },
    Fused { attempts: u32 },
}

/// 单 worker 监督器（崩溃计数 + 退避 + 熔断）。
#[derive(Debug, Clone)]
pub struct Supervisor {
    rule: RestartRule,
    attempts: u32,
}

impl Supervisor {
    pub fn new(rule: RestartRule) -> Self {
        Self { rule, attempts: 0 }
    }

    pub fn rule(&self) -> RestartRule {
        self.rule
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// 健康运行后复位崩溃计数。
    pub fn mark_healthy(&mut self) {
        self.attempts = 0;
    }

    /// 崩溃上报：返回监督状态（重启待退避，或熔断）。
    pub fn on_crash(&mut self) -> SupervisionState {
        self.attempts += 1;
        if self.attempts > self.rule.max_restarts {
            SupervisionState::Fused {
                attempts: self.attempts,
            }
        } else {
            SupervisionState::Restarting {
                attempts: self.attempts,
                next_retry_secs: backoff_secs(
                    self.rule.base_backoff_secs,
                    self.attempts.saturating_sub(1),
                ),
            }
        }
    }
}

/// 等待图环检测：返回首个环（首尾同一节点），无环返回 `None`。
pub fn detect_cycle(edges: &[(AgentId, AgentId)]) -> Option<Vec<AgentId>> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Color {
        White,
        Gray,
        Black,
    }

    fn visit(
        node: &str,
        adj: &HashMap<String, Vec<String>>,
        color: &mut HashMap<String, Color>,
        stack: &mut Vec<String>,
        cycle: &mut Vec<AgentId>,
    ) -> bool {
        color.insert(node.to_string(), Color::Gray);
        stack.push(node.to_string());
        if let Some(nexts) = adj.get(node) {
            for next in nexts {
                let c = color.get(next).copied().unwrap_or(Color::White);
                if c == Color::White {
                    if visit(next, adj, color, stack, cycle) {
                        return true;
                    }
                } else if c == Color::Gray {
                    let start = stack.iter().position(|n| n == next).unwrap_or(0);
                    cycle.extend(stack[start..].iter().cloned());
                    cycle.push(next.clone());
                    return true;
                }
            }
        }
        stack.pop();
        color.insert(node.to_string(), Color::Black);
        false
    }

    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for (from, to) in edges {
        adj.entry(from.clone()).or_default().push(to.clone());
    }
    let keys: Vec<String> = adj.keys().cloned().collect();
    let mut color = HashMap::new();
    let mut stack = Vec::new();
    let mut cycle = Vec::new();
    for node in keys {
        if color.get(&node).copied().unwrap_or(Color::White) == Color::White
            && visit(&node, &adj, &mut color, &mut stack, &mut cycle)
        {
            return Some(cycle);
        }
    }
    None
}

/// 等待图边：`waiter` 正在等待 `waited` 完成（handoff 推广：任意 agent 间等待）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitEdge {
    pub waiter: AgentId,
    pub waited: AgentId,
    /// 等待超时（None = 无限等待，靠仲裁与整体预算兜底）。
    pub timeout: Option<Duration>,
}

impl WaitEdge {
    pub fn new(waiter: impl Into<AgentId>, waited: impl Into<AgentId>) -> Self {
        Self {
            waiter: waiter.into(),
            waited: waited.into(),
            timeout: None,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

/// 等待图环检测：把 `plan.rs` 的 DAG 环检测推广为 agent 间等待图检测。
/// 返回构成环的 agent 序列（首尾同一节点）；无环返回 `None`。
pub fn detect_wait_cycle(edges: &[WaitEdge]) -> Option<Vec<AgentId>> {
    let pairs: Vec<(AgentId, AgentId)> = edges
        .iter()
        .map(|e| (e.waiter.clone(), e.waited.clone()))
        .collect();
    detect_cycle(&pairs)
}

/// 环仲裁：按优先级取消最低优先分支（priority 值越大优先级越低；缺省视为最低）。
/// 并列取字典序最大者，保证确定性。
pub fn arbitrate_wait_cycle(cycle: &[AgentId], priority: &HashMap<AgentId, u32>) -> AgentId {
    cycle
        .iter()
        .filter(|a| !a.is_empty())
        .max_by(|a, b| {
            let pa = priority.get(*a).copied().unwrap_or(u32::MAX);
            let pb = priority.get(*b).copied().unwrap_or(u32::MAX);
            pa.cmp(&pb).then_with(|| a.cmp(b))
        })
        .cloned()
        .unwrap_or_else(|| "unknown".to_string())
}

/// 等待图仲裁决议：取消哪个分支以解开死锁。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitResolution {
    pub cancel: AgentId,
    pub cycle: Vec<AgentId>,
    pub reason: String,
}

/// 可维护的等待图：登记边与优先级，周期扫描环并给出仲裁决议。
#[derive(Debug, Clone, Default)]
pub struct WaitGraph {
    edges: Vec<WaitEdge>,
    priority: HashMap<AgentId, u32>,
}

impl WaitGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(
        &mut self,
        waiter: impl Into<AgentId>,
        waited: impl Into<AgentId>,
        timeout: Option<Duration>,
    ) {
        self.edges.push(WaitEdge {
            waiter: waiter.into(),
            waited: waited.into(),
            timeout,
        });
    }

    pub fn set_priority(&mut self, agent: impl Into<AgentId>, priority: u32) {
        self.priority.insert(agent.into(), priority);
    }

    pub fn edges(&self) -> &[WaitEdge] {
        &self.edges
    }

    /// 周期扫描：发现环返回环路径。
    pub fn cycle(&self) -> Option<Vec<AgentId>> {
        detect_wait_cycle(&self.edges)
    }

    /// 仲裁：发现环即按优先级取消低优先分支（带超时等待的边优先由超时处理）。
    pub fn resolve(&self) -> Option<WaitResolution> {
        let cycle = self.cycle()?;
        let cancel = arbitrate_wait_cycle(&cycle, &self.priority);
        Some(WaitResolution {
            reason: format!(
                "agent 等待图死锁：取消低优先分支 {cancel}（环：{}）",
                cycle.join(" → ")
            ),
            cycle,
            cancel,
        })
    }
}

/// 消息去重键：correlation_id + 消息种类 + payload 摘要（at-least-once 去重）。
/// 同一键重复出现 = 重复消息（可检测、可幂等丢弃）。
pub fn message_dedup_key(msg: &BusMessage) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    msg.correlation_id.hash(&mut hasher);
    (msg.kind as u8).hash(&mut hasher);
    if let Ok(bytes) = serde_json::to_vec(&msg.payload) {
        bytes.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// 保序去重：重复（同 correlation_id + 种类 + 载荷）消息只保留第一条。
pub fn dedupe_messages(msgs: &[BusMessage]) -> Vec<BusMessage> {
    let mut seen = std::collections::HashSet::new();
    msgs.iter()
        .filter(|msg| seen.insert(message_dedup_key(msg)))
        .cloned()
        .collect()
}

/// 把传输任务转为总线消息（Task 种类；bus_store 持久化/重放格式）。
/// 载荷携带 task_id/input/lineage，供断点恢复时按血缘重算。
pub fn transport_task_message(task: &TransportTask) -> BusMessage {
    BusMessage {
        id: 0,
        from: CONTROL_PLANE_AGENT.to_string(),
        to: task.worker.clone(),
        kind: MessageKind::Task,
        correlation_id: task.correlation_id.clone(),
        payload: serde_json::json!({
            "task_id": task.task_id,
            "input": task.input,
            "lineage": task.lineage,
        }),
    }
}

/// 控制面 agent 标识（总线 from 字段）。
pub const CONTROL_PLANE_AGENT: &str = "control-plane";

/// 节点注册消息（总线持久化/重放格式）：负载携带 node_id + CapabilityCard。
pub fn register_node_message(
    node_id: &str,
    card: &crate::capability::CapabilityCard,
) -> BusMessage {
    BusMessage {
        id: 0,
        from: CONTROL_PLANE_AGENT.to_string(),
        to: CONTROL_PLANE_AGENT.to_string(),
        kind: MessageKind::Task,
        correlation_id: format!("node:register:{node_id}"),
        payload: serde_json::json!({
            "node_id": node_id,
            "card": card,
        }),
    }
}

/// 任务经 transport 提交，关键消息先经总线持久化：
/// 失败/恢复语义沿用 BusStore 重放（`replay_store` 重新投递 + 接收方幂等去重）。
pub async fn submit_via_bus_and_transport(
    bus: &AgentBus,
    transport: &Arc<dyn FleetTransport>,
    task: TransportTask,
) -> Result<(), String> {
    let msg = transport_task_message(&task);
    bus.send(
        msg.from.clone(),
        msg.to.clone(),
        msg.kind,
        msg.correlation_id.clone(),
        msg.payload.clone(),
        OverflowPolicy::Reject,
    )
    .await
    .map_err(|e| e.to_string())?;
    transport.submit(task).await
}

/// fan-out 单 worker 终态（部分成功仲裁的依据）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FanOutStatus {
    /// 成功产出。
    Succeeded,
    /// worker 明确失败（确定性错误，可单独重试）。
    #[default]
    Failed,
    /// 单 worker 超时（可单独重试）。
    TimedOut,
    /// 取消传播（调用方取消，不自动重试）。
    Cancelled,
    /// 整体预算硬停（时长维度）。
    Aborted,
    /// 能力不满足/降级（未调度；确定性，不自动重试）。
    Unfit,
}

/// fan-out 单 worker 结果（按输入顺序返回）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FanOutOutcome {
    pub worker: AgentId,
    pub ok: bool,
    #[serde(default)]
    pub status: FanOutStatus,
    pub error: Option<String>,
    pub output: Option<String>,
}

impl FanOutOutcome {
    pub fn success(worker: impl Into<AgentId>, output: impl Into<String>) -> Self {
        Self {
            worker: worker.into(),
            ok: true,
            status: FanOutStatus::Succeeded,
            error: None,
            output: Some(output.into()),
        }
    }

    pub fn failure(worker: impl Into<AgentId>, error: impl Into<String>) -> Self {
        Self {
            worker: worker.into(),
            ok: false,
            status: FanOutStatus::Failed,
            error: Some(error.into()),
            output: None,
        }
    }

    pub fn with_status(mut self, status: FanOutStatus) -> Self {
        self.ok = status == FanOutStatus::Succeeded;
        self.status = status;
        self
    }
}

/// fan-out 调度配置：并行度、预算、单 worker 超时与取消传播。
#[derive(Debug, Clone)]
pub struct FanOutConfig {
    pub max_parallel: usize,
    pub budget: Budget,
    /// 单 worker 超时（None = 不超时，仅受整体预算约束）。
    pub per_worker_timeout: Option<Duration>,
    /// 取消标志：置位后不再启动新 worker，在飞 worker 被 abort，未完成者标记 Cancelled。
    pub cancelled: Option<Arc<AtomicBool>>,
    /// 能力注册表（可选）：需求不满足/降级的 worker 标记 [`FanOutStatus::Unfit`] 且不调度。
    pub capabilities: Option<crate::capability::CapabilityWorkerRegistry>,
    /// 能力需求（`capabilities` 提供时生效；worker 逐一评估）。
    pub requirement: Option<crate::capability::WorkerRequirement>,
    /// 经验库（可选）：每个终态结果以 `correlation_id:worker` 幂等写入。
    pub experience: Option<crate::experience_store::ExperienceStore>,
}

impl Default for FanOutConfig {
    fn default() -> Self {
        Self {
            max_parallel: 4,
            budget: Budget::default(),
            per_worker_timeout: None,
            cancelled: None,
            capabilities: None,
            requirement: None,
            experience: None,
        }
    }
}

/// fan-out 汇总报告（部分成功仲裁 + 可单独重试视图）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FanOutReport {
    pub correlation_id: CorrelationId,
    /// 按输入顺序的结果。
    pub outcomes: Vec<FanOutOutcome>,
}

impl FanOutReport {
    pub fn outcome(&self, worker: &str) -> Option<&FanOutOutcome> {
        self.outcomes.iter().find(|o| o.worker == worker)
    }

    pub fn succeeded(&self) -> Vec<&FanOutOutcome> {
        self.outcomes
            .iter()
            .filter(|o| o.status == FanOutStatus::Succeeded)
            .collect()
    }

    pub fn failed(&self) -> Vec<&FanOutOutcome> {
        self.outcomes
            .iter()
            .filter(|o| o.status != FanOutStatus::Succeeded)
            .collect()
    }

    /// 可单独重试的子任务（明确失败或超时；取消/预算中止由调用方决策，不自动重试）。
    pub fn retryable(&self) -> Vec<&FanOutOutcome> {
        self.outcomes
            .iter()
            .filter(|o| matches!(o.status, FanOutStatus::Failed | FanOutStatus::TimedOut))
            .collect()
    }

    /// 部分成功：已成功结果保留，未成功者是否为空。
    pub fn all_succeeded(&self) -> bool {
        self.failed().is_empty()
    }
}

/// fan-out 增强版：超时 + 取消传播 + 部分成功仲裁。
///
/// - 超时：`config.per_worker_timeout` 到点 abort 该 worker（tokio timeout 取消 future）。
/// - 取消：`config.cancelled` 置位后，未启动者直接 Cancelled，在飞者 abort，已成功结果保留。
/// - 预算：时长维度整体硬停（与 [`Budget::exceeded`] 语义一致），未完成者 Aborted。
/// - 结果按输入顺序返回；已成功结果不受后续取消/超时影响（部分成功保留）。
pub async fn fan_out_cfg<F, Fut>(
    workers: &[AgentId],
    config: FanOutConfig,
    correlation_id: impl Into<CorrelationId>,
    run: F,
) -> FanOutReport
where
    F: Fn(AgentId) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<String, String>> + Send + 'static,
{
    // 先落成具体类型：后续 clone 进 'static 任务不受泛型生命周期约束。
    let correlation_id: CorrelationId = correlation_id.into();
    let max_parallel = config.max_parallel.max(1);
    let run = Arc::new(run);
    let start = Instant::now();
    let mut set: JoinSet<(AgentId, Result<String, String>, bool)> = JoinSet::new();
    let mut outcomes: HashMap<AgentId, FanOutOutcome> = HashMap::new();
    let mut next = 0usize;
    let mut aborted_by_budget = false;
    let mut cancelled_by_flag = false;
    let mut panic_seen = false;

    loop {
        let cancelled = config
            .cancelled
            .as_ref()
            .map(|c| c.load(Ordering::SeqCst))
            .unwrap_or(false);
        let budget_hit = config.budget.exceeded(0, 0, start.elapsed());
        if budget_hit {
            aborted_by_budget = true;
        }
        if cancelled {
            cancelled_by_flag = true;
        }
        if aborted_by_budget || cancelled_by_flag {
            if !set.is_empty() {
                set.abort_all();
            }
            while set.join_next().await.is_some() {}
            break;
        }
        while set.len() < max_parallel && next < workers.len() {
            let worker = workers[next].clone();
            next += 1;
            // 能力过滤：需求不满足/降级的 worker 明确标记 Unfit，不调度。
            if let (Some(reg), Some(req)) = (&config.capabilities, &config.requirement) {
                let unfit = match reg.evaluate_worker(&worker, req) {
                    None => Some(format!("worker {worker} 未注册能力卡")),
                    Some(CapabilityMatch::Full) => None,
                    Some(CapabilityMatch::Partial { missing }) => Some(format!(
                        "worker {worker} 能力降级（缺失 {}）",
                        missing.join(", ")
                    )),
                    Some(CapabilityMatch::Unfit { reasons }) => Some(format!(
                        "worker {worker} 能力不满足：{}",
                        reasons.join("；")
                    )),
                };
                if let Some(reason) = unfit {
                    if let Some(reg) = &config.capabilities {
                        reg.mark_health(&worker, false);
                    }
                    let outcome = FanOutOutcome::failure(worker.clone(), reason)
                        .with_status(FanOutStatus::Unfit);
                    if let Some(exp) = &config.experience {
                        let corr: CorrelationId = correlation_id.clone();
                        record_fanout_experience(exp, corr, &outcome);
                    }
                    outcomes.insert(worker, outcome);
                    continue;
                }
            }
            let task_worker = worker.clone();
            let run = Arc::clone(&run);
            let per_timeout = config.per_worker_timeout;
            let exp = config.experience.clone();
            let caps = config.capabilities.clone();
            let corr: CorrelationId = correlation_id.clone();
            set.spawn(async move {
                let result = match per_timeout {
                    Some(timeout) => {
                        match tokio::time::timeout(timeout, run(task_worker.clone())).await {
                            Ok(result) => (result, false),
                            Err(_) => (Err("worker timed out".to_string()), true),
                        }
                    }
                    None => {
                        let id = task_worker.clone();
                        (run(id).await, false)
                    }
                };
                if let Some(exp) = exp {
                    let mut outcome = match &result.0 {
                        Ok(output) => FanOutOutcome::success(task_worker.clone(), output.clone()),
                        Err(err) => FanOutOutcome::failure(task_worker.clone(), err.clone()),
                    };
                    if result.1 {
                        outcome = outcome.with_status(FanOutStatus::TimedOut);
                    }
                    record_fanout_experience(&exp, corr, &outcome);
                }
                if let Some(reg) = &caps {
                    reg.mark_health(&task_worker, result.0.is_ok());
                }
                (task_worker, result.0, result.1)
            });
        }
        if set.is_empty() {
            break;
        }
        match set.join_next().await {
            Some(Ok((worker, result, timed_out))) => {
                let mut outcome = match result {
                    Ok(output) => FanOutOutcome::success(worker, output),
                    Err(err) => FanOutOutcome::failure(worker, err),
                };
                if timed_out {
                    outcome = outcome.with_status(FanOutStatus::TimedOut);
                }
                outcomes.insert(outcome.worker.clone(), outcome);
            }
            Some(Err(_)) => panic_seen = true,
            None => break,
        }
    }

    let report = workers
        .iter()
        .map(|worker| {
            if let Some(outcome) = outcomes.remove(worker) {
                outcome
            } else if aborted_by_budget {
                FanOutOutcome::failure(worker, "budget exceeded: task aborted")
                    .with_status(FanOutStatus::Aborted)
            } else if cancelled_by_flag {
                FanOutOutcome::failure(worker, "cancelled by caller")
                    .with_status(FanOutStatus::Cancelled)
            } else if panic_seen {
                FanOutOutcome::failure(worker, "worker panicked or join error")
            } else {
                FanOutOutcome::failure(worker, "worker did not complete")
            }
        })
        .collect();
    FanOutReport {
        correlation_id,
        outcomes: report,
    }
}

/// 把 fan-out 终态结果幂等写入经验库（correlation_id = `fan-out:correlation_id:worker`）。
fn record_fanout_experience(
    exp: &ExperienceStore,
    correlation_id: CorrelationId,
    o: &FanOutOutcome,
) {
    let outcome = match o.status {
        FanOutStatus::Succeeded => Outcome::Success,
        FanOutStatus::Cancelled => Outcome::Cancelled,
        FanOutStatus::Aborted | FanOutStatus::Unfit => Outcome::Aborted,
        FanOutStatus::Failed | FanOutStatus::TimedOut => Outcome::Failure,
    };
    let attribution = Attribution {
        goal_id: None,
        plan_id: None,
        step_id: None,
        input_keys: Vec::new(),
        error: o.error.clone(),
    };
    let _ = exp.record_worker_outcome(
        format!("fan-out:{correlation_id}:{}", o.worker),
        o.worker.clone(),
        outcome,
        attribution,
    );
}

/// 数据并行 fan-out：`max_parallel` 限流 + 预算硬停（时长维度），结果按输入顺序返回。
///
/// 预算的轮次/步数维度由 worker 自身循环检查（本函数只负责调度与时长预算）。
/// worker 闭包不应 panic；若发生，本函数保守地将未完成 worker 标记为失败。
/// 需要超时/取消/部分成功仲裁时使用 [`fan_out_cfg`]。
pub async fn fan_out<F, Fut>(
    workers: &[AgentId],
    max_parallel: usize,
    budget: Budget,
    run: F,
) -> Vec<FanOutOutcome>
where
    F: Fn(AgentId) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<String, String>> + Send + 'static,
{
    fan_out_cfg(
        workers,
        FanOutConfig {
            max_parallel,
            budget,
            ..Default::default()
        },
        "fan-out",
        run,
    )
    .await
    .outcomes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn msg(id: u64, kind: MessageKind) -> BusMessage {
        BusMessage {
            id,
            from: "a".to_string(),
            to: "b".to_string(),
            kind,
            correlation_id: "c1".to_string(),
            payload: serde_json::Value::Null,
        }
    }

    #[test]
    fn mailbox_overflow_drops_mergeable_keeps_critical() {
        let mut mb = Mailbox::new(2);
        assert_eq!(
            mb.push(msg(1, MessageKind::Task), OverflowPolicy::DropMergeable),
            Ok(PushOutcome::Pushed)
        );
        assert_eq!(
            mb.push(msg(2, MessageKind::Progress), OverflowPolicy::DropMergeable),
            Ok(PushOutcome::Pushed)
        );
        assert_eq!(
            mb.push(msg(3, MessageKind::Progress), OverflowPolicy::DropMergeable),
            Ok(PushOutcome::Dropped)
        );
        assert_eq!(
            mb.push(msg(4, MessageKind::Review), OverflowPolicy::DropMergeable),
            Err(BusError::MailboxFull(2))
        );
        assert_eq!(mb.len(), 2);
    }

    #[test]
    fn mailbox_reject_policy_returns_full_error() {
        let mut mb = Mailbox::new(1);
        assert_eq!(
            mb.push(msg(1, MessageKind::Progress), OverflowPolicy::Reject),
            Ok(PushOutcome::Pushed)
        );
        assert_eq!(
            mb.push(msg(2, MessageKind::Progress), OverflowPolicy::Reject),
            Err(BusError::MailboxFull(1))
        );
    }

    #[tokio::test]
    async fn bus_send_drain_and_unregister() {
        let bus = AgentBus::new();
        bus.register("worker-a", 4).await;
        bus.register("worker-b", 4).await;
        let id = bus
            .send(
                "core",
                "worker-a",
                MessageKind::Task,
                "corr-1",
                serde_json::json!({"q":1}),
                OverflowPolicy::Reject,
            )
            .await
            .expect("delivered");
        assert_eq!(bus.pending("worker-a").await, 1);
        let drained = bus.drain("worker-a").await;
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].id, id);
        assert_eq!(drained[0].correlation_id, "corr-1");
        assert!(bus.unregister("worker-a").await);
        assert!(!bus.contains("worker-a").await);
    }

    #[tokio::test]
    async fn bus_send_unknown_agent_errors() {
        let bus = AgentBus::new();
        bus.register("worker-a", 4).await;
        let err = bus
            .send(
                "core",
                "nobody",
                MessageKind::Task,
                "c",
                serde_json::Value::Null,
                OverflowPolicy::Reject,
            )
            .await
            .unwrap_err();
        assert_eq!(err, BusError::UnknownAgent("nobody".to_string()));
    }

    #[tokio::test]
    async fn broadcast_delivers_to_registered_agents() {
        let bus = AgentBus::new();
        bus.register("w1", 4).await;
        bus.register("w2", 4).await;
        let delivered = bus
            .broadcast(
                "core",
                "topic",
                MessageKind::Progress,
                "corr-x",
                serde_json::json!({"n":1}),
                OverflowPolicy::Reject,
            )
            .await;
        assert_eq!(delivered.len(), 2);
        assert!(delivered.contains(&"w1".to_string()));
        assert!(delivered.contains(&"w2".to_string()));
        assert_eq!(bus.pending("w1").await, 1);
        assert_eq!(bus.pending("w2").await, 1);
    }

    #[test]
    fn budget_exceeded_flags() {
        let b = Budget {
            max_turns: 2,
            max_steps: 5,
            max_duration_secs: 10,
        };
        assert!(!b.exceeded(1, 1, Duration::from_secs(1)));
        assert!(b.exceeded(2, 1, Duration::from_secs(1)));
        assert!(b.exceeded(1, 5, Duration::from_secs(1)));
        assert!(b.exceeded(1, 1, Duration::from_secs(10)));
    }

    #[test]
    fn backoff_secs_grows_and_caps() {
        assert_eq!(backoff_secs(1, 0), 1);
        assert_eq!(backoff_secs(1, 2), 4);
        assert_eq!(backoff_secs(2, 3), 16);
        assert_eq!(backoff_secs(1, 100), 60);
    }

    #[test]
    fn supervisor_fuses_after_max_restarts() {
        let rule = RestartRule {
            max_restarts: 2,
            base_backoff_secs: 1,
            policy: RestartPolicy::OneForOne,
        };
        let mut sup = Supervisor::new(rule);
        assert_eq!(
            sup.on_crash(),
            SupervisionState::Restarting {
                attempts: 1,
                next_retry_secs: 1
            }
        );
        assert_eq!(
            sup.on_crash(),
            SupervisionState::Restarting {
                attempts: 2,
                next_retry_secs: 2
            }
        );
        assert_eq!(sup.on_crash(), SupervisionState::Fused { attempts: 3 });
        assert_eq!(sup.attempts(), 3);
    }

    #[test]
    fn supervisor_resets_on_healthy() {
        let mut sup = Supervisor::new(RestartRule::default());
        let _ = sup.on_crash();
        sup.mark_healthy();
        assert_eq!(sup.attempts(), 0);
        assert_eq!(
            sup.on_crash(),
            SupervisionState::Restarting {
                attempts: 1,
                next_retry_secs: 1
            }
        );
    }

    #[test]
    fn detect_cycle_finds_handoff_loop() {
        let edges: Vec<(AgentId, AgentId)> = vec![
            ("a".into(), "b".into()),
            ("b".into(), "c".into()),
            ("c".into(), "a".into()),
        ];
        let cycle = detect_cycle(&edges).expect("cycle found");
        assert!(cycle.len() >= 3);
        assert_eq!(cycle.first(), cycle.last());
    }

    #[test]
    fn detect_cycle_clean_dag_none() {
        let edges: Vec<(AgentId, AgentId)> = vec![
            ("a".into(), "b".into()),
            ("a".into(), "c".into()),
            ("b".into(), "d".into()),
        ];
        assert_eq!(detect_cycle(&edges), None);
    }

    #[tokio::test]
    async fn fan_out_respects_max_parallel() {
        let workers: Vec<AgentId> = vec!["w1".into(), "w2".into(), "w3".into(), "w4".into()];
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let active_clone = Arc::clone(&active);
        let peak_clone = Arc::clone(&peak);
        let outcomes = fan_out(&workers, 2, Budget::default(), move |_id| {
            let active = Arc::clone(&active_clone);
            let peak = Arc::clone(&peak_clone);
            async move {
                let now = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                peak.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                Ok("done".to_string())
            }
        })
        .await;
        assert!(peak.load(std::sync::atomic::Ordering::SeqCst) <= 2);
        assert!(outcomes.iter().all(|o| o.ok));
        assert_eq!(outcomes.len(), 4);
    }

    #[tokio::test]
    async fn fan_out_partial_failure_and_order() {
        let workers: Vec<AgentId> = vec!["w1".into(), "w2".into(), "w3".into()];
        let outcomes = fan_out(&workers, 2, Budget::default(), |id| async move {
            if id == "w2" {
                Err("boom".to_string())
            } else {
                Ok(format!("ok:{id}"))
            }
        })
        .await;
        assert_eq!(outcomes.len(), 3);
        assert_eq!(outcomes[0].worker, "w1");
        assert!(outcomes[0].ok);
        assert_eq!(outcomes[0].output.as_deref(), Some("ok:w1"));
        assert_eq!(outcomes[1].worker, "w2");
        assert!(!outcomes[1].ok);
        assert_eq!(outcomes[1].error.as_deref(), Some("boom"));
        assert_eq!(outcomes[2].worker, "w3");
        assert!(outcomes[2].ok);
    }

    #[tokio::test]
    async fn fan_out_zero_budget_aborts_all() {
        let workers: Vec<AgentId> = vec!["w1".into(), "w2".into()];
        let budget = Budget {
            max_turns: 50,
            max_steps: 1000,
            max_duration_secs: 0,
        };
        let outcomes = fan_out(
            &workers,
            2,
            budget,
            |id| async move { Ok(format!("ok:{id}")) },
        )
        .await;
        assert!(outcomes.iter().all(|o| !o.ok));
        assert!(outcomes
            .iter()
            .all(|o| o.error.as_deref() == Some("budget exceeded: task aborted")));
    }
}
