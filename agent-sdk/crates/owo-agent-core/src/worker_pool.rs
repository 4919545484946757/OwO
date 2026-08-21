// R10:worker_pool 完成（租约/fencing 挂接），待主控接线
//! worker 子进程池（多 Agent P1）：把 worker 从进程内扩展到独立子进程。
//!
//! 设计来源：《多Agent并行体系-生产级设计与跨机扩展-2026-08-16.md》§2 任务模型 与 §4 可靠性：
//! - **隔离**：每个 worker 独立子进程；`IsolationMode::Sandbox` 为 OS 级沙箱接入点（Agent 3 沙箱实现）。
//! - **崩溃自愈**：心跳检测 + 指数退避重启（复用 `fleet::backoff_secs`，封顶 60s）+ 连续失败熔断
//!   （复用 `fleet::Supervisor`；健康任务完成后复位计数）。
//! - **预算**：轮次/时长在池侧强制（策略字段）；内存/CPU 上限本轮仅表达，OS 强制由沙箱实现。
//! - **协议**：受限 stdin/stdout + JSON 行结构化消息，禁止自由文本串线；stderr 供人读诊断。
//! - **清理**：`kill`/`shutdown`/`Drop` 均终止子进程；`Drop` 是安全网（同步 start_kill）。
//! - **事件**：崩溃/重启/熔断/预算中止/取消经 `fleet::WorkerEvent` 进入总线与审计。
//!
//! 生命周期：`spawn`（ready 握手）→ `submit`（结构化任务）→ `check_health`（心跳自愈）→
//! `cancel_pending`/`cancel_all`（取消传播）→ `kill`/`shutdown`（终止与清理）。

use crate::audit::AuditLog;
use crate::fleet::{
    new_correlation_id, AgentBus, AgentId, CorrelationId, RestartRule, SupervisionState,
    Supervisor, WorkerEvent, WorkerEventKind,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};
use tokio::task::JoinHandle;

pub type WorkerId = String;

/// ready 握手超时。
const READY_TIMEOUT: Duration = Duration::from_secs(15);
/// 心跳（ping/pong）超时。
const PING_TIMEOUT: Duration = Duration::from_secs(3);
/// 状态轮询间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(20);
/// 事件日志上限（最近 N 条）。
const EVENT_CAP: usize = 200;

/// 隔离模式：本轮 `Process`（进程隔离）；`Sandbox` 为 Agent 3 OS 级沙箱接入点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationMode {
    /// 独立子进程隔离（本轮默认；资源上限为策略字段，OS 强制待沙箱接入）。
    #[default]
    Process,
    /// 经 OS 级沙箱执行（Job Object/AppContainer 等；由 Agent 3 沙箱实现接入）。
    Sandbox,
}

/// worker 预算（策略字段；轮次/时长池侧强制，内存/CPU 本轮仅表达，OS 强制由沙箱实现）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WorkerBudget {
    /// 最大任务轮次（0 = 不限）。
    pub max_turns: u32,
    /// 最大运行时长（秒，0 = 不限；超时中止并 kill）。
    pub max_duration_secs: u64,
    /// 内存上限（MB；本轮为策略字段，沙箱接入后 OS 强制）。
    pub max_memory_mb: u64,
    /// CPU 核数上限（本轮为策略字段，沙箱接入后 OS 强制）。
    pub max_cpu_cores: f32,
}

impl Default for WorkerBudget {
    fn default() -> Self {
        Self {
            max_turns: 0,
            max_duration_secs: 0,
            max_memory_mb: 0,
            max_cpu_cores: 0.0,
        }
    }
}

impl WorkerBudget {
    pub fn exceeded(&self, turns: u32, elapsed: Duration) -> bool {
        (self.max_turns > 0 && turns >= self.max_turns)
            || (self.max_duration_secs > 0 && elapsed.as_secs() >= self.max_duration_secs)
    }
}

/// worker 进程规格。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerSpec {
    /// 注册名（总线/审计中的 worker 标识）。
    pub id: WorkerId,
    pub command: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// 环境变量白名单（仅这些变量传入子进程；其余一律不继承）。
    #[serde(default)]
    pub env_whitelist: Vec<(String, String)>,
    #[serde(default)]
    pub budget: WorkerBudget,
    #[serde(default)]
    pub isolation: IsolationMode,
    /// 崩溃重启规则（指数退避 + 熔断）。
    #[serde(default)]
    pub restart_rule: RestartRule,
}

impl WorkerSpec {
    pub fn new(id: impl Into<WorkerId>, command: impl Into<PathBuf>) -> Self {
        Self {
            id: id.into(),
            command: command.into(),
            args: Vec::new(),
            cwd: None,
            env_whitelist: Vec::new(),
            budget: WorkerBudget::default(),
            isolation: IsolationMode::Process,
            restart_rule: RestartRule::default(),
        }
    }

    pub fn args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn env_whitelist(mut self, env_whitelist: Vec<(String, String)>) -> Self {
        self.env_whitelist = env_whitelist;
        self
    }

    pub fn budget(mut self, budget: WorkerBudget) -> Self {
        self.budget = budget;
        self
    }

    pub fn isolation(mut self, isolation: IsolationMode) -> Self {
        self.isolation = isolation;
        self
    }

    pub fn restart_rule(mut self, restart_rule: RestartRule) -> Self {
        self.restart_rule = restart_rule;
        self
    }
}

/// worker 运行状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerStatus {
    Starting,
    Running,
    Restarting { attempts: u32, next_retry_secs: u64 },
    Fused { attempts: u32 },
    Stopped,
}

impl fmt::Display for WorkerStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Starting => write!(f, "starting"),
            Self::Running => write!(f, "running"),
            Self::Restarting {
                attempts,
                next_retry_secs,
            } => write!(
                f,
                "restarting(attempt={attempts}, backoff={next_retry_secs}s)"
            ),
            Self::Fused { attempts } => write!(f, "fused(attempt={attempts})"),
            Self::Stopped => write!(f, "stopped"),
        }
    }
}

/// 取消传播的内部标记（区别于真实 worker 错误文本）。
const CANCELLED_MARKER: &str = "__owo_cancelled__";

/// 池错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoolError {
    UnknownWorker(WorkerId),
    Spawn(String),
    NotReady(WorkerId),
    Timeout(String),
    Protocol(String),
    Fused(WorkerId),
    Stopped(WorkerId),
    BudgetDuration { worker: WorkerId, reason: String },
    BudgetTurns { worker: WorkerId, max_turns: u32 },
    WorkerFailed(String),
    Cancelled(WorkerId),
    Io(String),
}

impl fmt::Display for PoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownWorker(id) => write!(f, "未知 worker：{id}"),
            Self::Spawn(reason) => write!(f, "spawn 失败：{reason}"),
            Self::NotReady(id) => write!(f, "worker {id} 未就绪"),
            Self::Timeout(reason) => write!(f, "超时：{reason}"),
            Self::Protocol(reason) => write!(f, "协议错误：{reason}"),
            Self::Fused(id) => write!(f, "worker {id} 已熔断"),
            Self::Stopped(id) => write!(f, "worker {id} 已停止"),
            Self::BudgetDuration { worker, reason } => {
                write!(f, "worker {worker} 预算中止：{reason}")
            }
            Self::BudgetTurns { worker, max_turns } => {
                write!(f, "worker {worker} 预算中止：轮次上限 {max_turns}")
            }
            Self::WorkerFailed(reason) => write!(f, "worker 返回错误：{reason}"),
            Self::Cancelled(id) => write!(f, "worker {id} 任务被取消"),
            Self::Io(reason) => write!(f, "IO 错误：{reason}"),
        }
    }
}

impl Error for PoolError {}

/// 子进程回报（reader 任务 → 调度循环）。携带 `gen`（spawn 代数），
/// 调度循环忽略过期代消息（防旧 child 的 Exited/Ready 污染新 child）。
#[derive(Debug)]
enum ChildOutcome {
    Ready {
        worker: WorkerId,
        gen: u64,
    },
    Pong {
        worker: WorkerId,
        gen: u64,
    },
    Result {
        worker: WorkerId,
        gen: u64,
        task_id: String,
        result: Result<String, String>,
    },
    Exited {
        worker: WorkerId,
        gen: u64,
    },
    BadLine {
        worker: WorkerId,
        gen: u64,
    },
}

/// 子进程 → 父进程的结构化消息（JSON 行，tag="type"）。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ChildMsg {
    Ready,
    Pong,
    Result {
        task_id: String,
        ok: bool,
        #[serde(default)]
        output: Option<String>,
        #[serde(default)]
        error: Option<String>,
    },
}

/// 解析子进程一行输出；非 JSON / 未知类型 → Err（协议强制结构化，禁止自由文本串线）。
fn parse_child_line(line: &str) -> Result<ChildMsg, String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err("空行".to_string());
    }
    serde_json::from_str::<ChildMsg>(trimmed).map_err(|e| format!("非结构化消息：{e}"))
}

fn outcome_from_msg(worker: &WorkerId, gen: u64, msg: ChildMsg) -> ChildOutcome {
    match msg {
        ChildMsg::Ready => ChildOutcome::Ready {
            worker: worker.clone(),
            gen,
        },
        ChildMsg::Pong => ChildOutcome::Pong {
            worker: worker.clone(),
            gen,
        },
        ChildMsg::Result {
            task_id,
            ok,
            output,
            error,
        } => {
            let result = if ok {
                Ok(output.unwrap_or_default())
            } else {
                Err(error.unwrap_or_else(|| "未知错误".to_string()))
            };
            ChildOutcome::Result {
                worker: worker.clone(),
                gen,
                task_id,
                result,
            }
        }
    }
}

// ---------- 结构化父→子消息（JSON 行） ----------

/// 结构化消息行：JSON + 行尾换行（子进程 `read_line` 以 `\n` 为消息边界；
/// 无换行会导致子进程阻塞等待，父进程写入永远不被消费）。
fn task_line(task_id: &str, correlation_id: &str, input: &serde_json::Value) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "cmd": "task",
            "task_id": task_id,
            "correlation_id": correlation_id,
            "input": input,
        })
    )
}

fn ping_line() -> String {
    format!("{}\n", serde_json::json!({ "cmd": "ping" }))
}

fn cancel_line(task_id: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({ "cmd": "cancel", "task_id": task_id })
    )
}

fn shutdown_line() -> String {
    format!("{}\n", serde_json::json!({ "cmd": "shutdown" }))
}

/// worker 槽位（池内状态）。
struct WorkerSlot {
    spec: WorkerSpec,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    pid: Option<u32>,
    supervisor: Supervisor,
    status: WorkerStatus,
    turns: u32,
    started_at: Option<Instant>,
    exited: bool,
    /// spawn 代数：每次（重新）spawn 递增；reader 消息带代数，过期消息被调度循环忽略。
    gen: u64,
    pending: HashMap<String, oneshot::Sender<Result<String, String>>>,
    ping: Option<oneshot::Sender<()>>,
    /// 非结构化行计数（协议纪律观测：持续增长说明子进程未走结构化协议）。
    bad_lines: u32,
    /// 最近一次非结构化行时间。
    last_bad_line_at: Option<Instant>,
}

impl WorkerSlot {
    fn started_deadline(&self) -> Option<Instant> {
        let secs = self.spec.budget.max_duration_secs;
        if secs == 0 {
            None
        } else {
            self.started_at.map(|t| t + Duration::from_secs(secs))
        }
    }
}

/// 池内部状态（actor 模型：调度循环独占写）。
struct PoolInner {
    workers: HashMap<WorkerId, WorkerSlot>,
    tx: mpsc::UnboundedSender<ChildOutcome>,
    bus: Option<AgentBus>,
    supervisor_agent: Option<AgentId>,
    audit: Option<Arc<Mutex<AuditLog>>>,
    events: VecDeque<WorkerEvent>,
    /// 可选租约管理器：worker 持有租约，submit 前 fencing 校验（epoch/token）。
    leases: Option<crate::lease::LeaseManager>,
}

/// worker 子进程池（Clone 共享同一池）。
#[derive(Clone)]
pub struct WorkerPool {
    inner: Arc<AsyncMutex<PoolInner>>,
}

impl fmt::Debug for WorkerPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Ok(inner) = self.inner.try_lock() {
            f.debug_map()
                .entries(inner.workers.iter().map(|(id, slot)| (id, &slot.status)))
                .finish()
        } else {
            f.write_str("WorkerPool { <locked> }")
        }
    }
}

impl Drop for WorkerPool {
    /// 安全网：池被丢弃时同步 kill 全部子进程（start_kill 为同步 API）。
    /// 调用方应先用 `shutdown`/`cancel_all` 收尾；本实现保证不残留孤儿进程。
    fn drop(&mut self) {
        // 仅当这是最后一个池克隆时兜底 kill（安全网）：Clone 共享同一 inner，
        // 若每次 Drop 都 kill，则 PoolWorker 等临时克隆被丢弃时会误杀存活子进程。
        if Arc::strong_count(&self.inner) > 1 {
            return;
        }
        if let Ok(mut inner) = self.inner.try_lock() {
            for slot in inner.workers.values_mut() {
                if let Some(child) = slot.child.as_mut() {
                    let _ = child.start_kill();
                }
            }
        }
    }
}

impl Default for WorkerPool {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkerPool {
    /// 新建池（必须在 tokio runtime 内调用；内部启动调度循环任务）。
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let inner = Arc::new(AsyncMutex::new(PoolInner {
            workers: HashMap::new(),
            tx,
            bus: None,
            supervisor_agent: None,
            audit: None,
            events: VecDeque::new(),
            leases: None,
        }));
        let dispatcher = Arc::clone(&inner);
        tokio::spawn(async move { Self::dispatch_loop(dispatcher, rx).await });
        Self { inner }
    }

    /// 挂接租约管理器：worker 持有租约（spawn 时 acquire，submit 前 fencing 校验，
    /// terminate/kill 时 release）。
    pub async fn attach_leases(&self, leases: crate::lease::LeaseManager) {
        let mut inner = self.inner.lock().await;
        inner.leases = Some(leases);
    }

    /// 注册进总线：崩溃/重启/熔断等事件发往 `supervisor_agent`（需先 register 该 agent）。
    pub async fn attach_bus(&self, bus: AgentBus, supervisor_agent: impl Into<AgentId>) {
        let mut inner = self.inner.lock().await;
        inner.bus = Some(bus);
        inner.supervisor_agent = Some(supervisor_agent.into());
    }

    /// 附加审计日志：每次生命周期事件写一条 `worker.<kind>` 记录。
    pub async fn attach_audit(&self, log: Arc<Mutex<AuditLog>>) {
        let mut inner = self.inner.lock().await;
        inner.audit = Some(log);
    }

    /// spawn 一个 worker：启动子进程 + ready 握手（结构化协议就绪）后返回。
    pub async fn spawn(&self, spec: WorkerSpec) -> Result<WorkerId, PoolError> {
        let id = spec.id.clone();
        self.spawn_inner(spec, id.clone(), true).await?;
        self.emit(
            &id,
            WorkerEventKind::Started,
            "worker 已启动".to_string(),
            new_correlation_id(),
        )
        .await;
        Ok(id)
    }

    pub async fn contains(&self, id: &str) -> bool {
        self.inner.lock().await.workers.contains_key(id)
    }

    pub async fn worker_count(&self) -> usize {
        self.inner.lock().await.workers.len()
    }

    pub async fn workers(&self) -> Vec<WorkerId> {
        self.inner.lock().await.workers.keys().cloned().collect()
    }

    pub async fn status(&self, id: &str) -> Option<WorkerStatus> {
        self.inner
            .lock()
            .await
            .workers
            .get(id)
            .map(|s| s.status.clone())
    }

    pub async fn pid(&self, id: &str) -> Option<u32> {
        self.inner.lock().await.workers.get(id).and_then(|s| s.pid)
    }

    /// 最近事件（上限 EVENT_CAP 条；总线/审计之外的本地视图）。
    pub async fn events(&self) -> Vec<WorkerEvent> {
        self.inner.lock().await.events.iter().cloned().collect()
    }

    /// 心跳：ping → pong（结构化协议活性探测）。
    pub async fn ping(&self, id: &str) -> Result<(), PoolError> {
        let rx = {
            let mut inner = self.inner.lock().await;
            let slot = inner
                .workers
                .get_mut(id)
                .ok_or_else(|| PoolError::UnknownWorker(id.to_string()))?;
            if slot.exited {
                return Err(PoolError::NotReady(id.to_string()));
            }
            let (tx, rx) = oneshot::channel();
            // 先注册 pong 期望，再写 ping：防"子进程极快回包但父进程尚未登记"的竞态。
            slot.ping = Some(tx);
            let write = async {
                let stdin = slot
                    .stdin
                    .as_mut()
                    .ok_or_else(|| PoolError::NotReady(id.to_string()))?;
                stdin
                    .write_all(ping_line().as_bytes())
                    .await
                    .map_err(|e| PoolError::Io(e.to_string()))?;
                stdin
                    .flush()
                    .await
                    .map_err(|e| PoolError::Io(e.to_string()))?;
                Ok::<(), PoolError>(())
            };
            // 写入也纳入超时：子进程不读 stdin 时不得让写侧挂死整个池。
            match tokio::time::timeout(PING_TIMEOUT, write).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    slot.ping = None;
                    return Err(e);
                }
                Err(_) => {
                    slot.ping = None;
                    return Err(PoolError::Timeout(format!("worker {id} 心跳写入超时")));
                }
            }
            rx
        };
        tokio::time::timeout(PING_TIMEOUT, rx)
            .await
            .map_err(|_| PoolError::Timeout(format!("worker {id} 心跳超时")))?
            .map_err(|_| PoolError::Protocol(format!("worker {id} pong 通道异常")))?;
        Ok(())
    }

    /// 健康检查：ping 失败/已退出 → 走崩溃自愈（指数退避重启 / 熔断）。
    pub async fn check_health(&self, id: &str) -> Result<WorkerStatus, PoolError> {
        let exited = self
            .inner
            .lock()
            .await
            .workers
            .get(id)
            .map(|s| s.exited)
            .unwrap_or(false);
        let alive = if exited {
            false
        } else {
            self.ping(id).await.is_ok()
        };
        if alive {
            Ok(WorkerStatus::Running)
        } else {
            self.handle_crash(id, "心跳/退出检测").await
        }
    }

    /// 提交结构化任务：发送 task 消息并等待结果（时长预算到期中止并 kill）。
    pub async fn submit(&self, id: &str, input: &serde_json::Value) -> Result<String, PoolError> {
        let (rx, correlation_id) = {
            let mut inner = self.inner.lock().await;
            // 租约 fencing 校验：worker 租约失效（过期/重连/分区只读）时拒绝提交。
            // 先取租约（不可变借用）再取 slot（可变借用），避免借用冲突。
            if let Some(leases) = inner.leases.clone() {
                match leases.lease(id) {
                    Some(lease) => {
                        if let Err(e) =
                            leases.verify_write(&lease.holder, &lease.token, lease.epoch)
                        {
                            return Err(PoolError::Io(format!("worker {id} fencing 拒绝：{e}")));
                        }
                    }
                    None => {
                        return Err(PoolError::Io(format!("worker {id} 无租约，拒绝提交")));
                    }
                }
            }
            let slot = inner
                .workers
                .get_mut(id)
                .ok_or_else(|| PoolError::UnknownWorker(id.to_string()))?;
            if matches!(slot.status, WorkerStatus::Fused { .. }) {
                return Err(PoolError::Fused(id.to_string()));
            }
            if slot.status == WorkerStatus::Stopped {
                return Err(PoolError::Stopped(id.to_string()));
            }
            if slot.exited {
                return Err(PoolError::NotReady(id.to_string()));
            }
            let max_turns = slot.spec.budget.max_turns;
            if max_turns > 0 && slot.turns >= max_turns {
                return Err(PoolError::BudgetTurns {
                    worker: id.to_string(),
                    max_turns,
                });
            }
            let task_id = uuid::Uuid::new_v4().to_string();
            let correlation_id = new_correlation_id();
            let line = task_line(&task_id, &correlation_id, input);
            let (tx, rx) = oneshot::channel();
            // 先登记 pending 再写 stdin：防"子进程极快回包但父进程尚未登记"的竞态丢结果。
            slot.pending.insert(task_id.clone(), tx);
            let write = async {
                let stdin = slot
                    .stdin
                    .as_mut()
                    .ok_or_else(|| PoolError::NotReady(id.to_string()))?;
                stdin
                    .write_all(line.as_bytes())
                    .await
                    .map_err(|e| PoolError::Io(e.to_string()))?;
                stdin
                    .flush()
                    .await
                    .map_err(|e| PoolError::Io(e.to_string()))?;
                Ok::<(), PoolError>(())
            };
            if let Err(e) = write.await {
                slot.pending.remove(&task_id);
                return Err(e);
            }
            slot.turns = slot.turns.saturating_add(1);
            (rx, correlation_id)
        };

        let deadline = {
            let inner = self.inner.lock().await;
            inner
                .workers
                .get(id)
                .ok_or_else(|| PoolError::UnknownWorker(id.to_string()))?
                .started_deadline()
        };
        let result = match deadline {
            Some(deadline) => {
                let now = Instant::now();
                if now >= deadline {
                    None
                } else {
                    tokio::time::timeout(deadline - now, rx).await.ok()
                }
            }
            None => Some(rx.await),
        };
        match result {
            Some(Ok(Ok(output))) => {
                self.mark_healthy(id).await;
                Ok(output)
            }
            Some(Ok(Err(err))) if err == CANCELLED_MARKER => {
                Err(PoolError::Cancelled(id.to_string()))
            }
            Some(Ok(Err(err))) => Err(PoolError::WorkerFailed(err)),
            Some(Err(_)) => Err(PoolError::NotReady(id.to_string())),
            None => {
                let reason = "worker 时长预算到期".to_string();
                self.abort_budget(id, &reason, &correlation_id).await;
                Err(PoolError::BudgetDuration {
                    worker: id.to_string(),
                    reason,
                })
            }
        }
    }

    /// 取消该 worker 的全部待处理任务（取消传播：pending 立即以 cancelled 解决；
    /// 子进程侧由 cancel 消息通知，阻塞中的任务随后 kill 清理）。返回取消数。
    pub async fn cancel_pending(&self, id: &str) -> Result<usize, PoolError> {
        let count = {
            let mut inner = self.inner.lock().await;
            let slot = inner
                .workers
                .get_mut(id)
                .ok_or_else(|| PoolError::UnknownWorker(id.to_string()))?;
            let count = slot.pending.len();
            let lines: Vec<String> = slot
                .pending
                .keys()
                .map(|task_id| cancel_line(task_id))
                .collect();
            if let Some(stdin) = slot.stdin.as_mut() {
                for line in &lines {
                    let _ = stdin.write_all(line.as_bytes()).await;
                    let _ = stdin.flush().await;
                }
            }
            for (_, tx) in slot.pending.drain() {
                let _ = tx.send(Err(CANCELLED_MARKER.to_string()));
            }
            count
        };
        if count > 0 {
            self.emit(
                id,
                WorkerEventKind::Cancelled,
                format!("取消 {count} 个待处理任务"),
                new_correlation_id(),
            )
            .await;
        }
        Ok(count)
    }

    /// 取消全部 worker 的待处理任务。
    pub async fn cancel_all(&self) -> usize {
        let ids = self.workers().await;
        let mut total = 0usize;
        for id in ids {
            if let Ok(n) = self.cancel_pending(&id).await {
                total += n;
            }
        }
        total
    }

    /// kill 单个 worker（终止子进程并回收；状态 Stopped）。
    pub async fn kill(&self, id: &str) -> Result<(), PoolError> {
        self.terminate(id, "worker 被 kill".to_string()).await;
        {
            let mut inner = self.inner.lock().await;
            let slot = inner
                .workers
                .get_mut(id)
                .ok_or_else(|| PoolError::UnknownWorker(id.to_string()))?;
            slot.status = WorkerStatus::Stopped;
        }
        self.emit(
            id,
            WorkerEventKind::Stopped,
            "kill".to_string(),
            new_correlation_id(),
        )
        .await;
        Ok(())
    }

    /// 手动重启（终止 + 立即按原规格拉起，不做退避；自动退避在 [`Self::check_health`]）。
    pub async fn restart(&self, id: &str) -> Result<WorkerStatus, PoolError> {
        self.terminate(id, "手动重启".to_string()).await;
        self.respawn(id).await?;
        Ok(WorkerStatus::Running)
    }

    /// 关闭全部 worker：发送 shutdown、kill、回收、标记 Stopped。
    pub async fn shutdown(&self) {
        let ids = self.workers().await;
        for id in ids {
            // 尽力发送 shutdown 协议消息。
            {
                let mut inner = self.inner.lock().await;
                if let Some(slot) = inner.workers.get_mut(&id) {
                    if let Some(stdin) = slot.stdin.as_mut() {
                        let _ = stdin.write_all(shutdown_line().as_bytes()).await;
                        let _ = stdin.flush().await;
                    }
                }
            }
            self.terminate(&id, "shutdown".to_string()).await;
            {
                let mut inner = self.inner.lock().await;
                if let Some(slot) = inner.workers.get_mut(&id) {
                    slot.status = WorkerStatus::Stopped;
                }
            }
            self.emit(
                &id,
                WorkerEventKind::Stopped,
                "shutdown".to_string(),
                new_correlation_id(),
            )
            .await;
        }
    }

    // ---------- 内部实现 ----------

    async fn emit(
        &self,
        worker: &str,
        kind: WorkerEventKind,
        detail: String,
        correlation_id: CorrelationId,
    ) {
        let event = WorkerEvent::new(worker, kind, detail, correlation_id);
        let (bus, supervisor, audit) = {
            let mut inner = self.inner.lock().await;
            inner.events.push_back(event.clone());
            if inner.events.len() > EVENT_CAP {
                inner.events.pop_front();
            }
            (
                inner.bus.clone(),
                inner.supervisor_agent.clone(),
                inner.audit.clone(),
            )
        };
        if let (Some(bus), Some(supervisor)) = (bus, supervisor) {
            let _ = bus.send_worker_event(worker, supervisor, &event).await;
        }
        if let Some(audit) = audit {
            if let Ok(mut log) = audit.lock() {
                log.record(
                    "worker_pool",
                    &format!("worker.{}", kind.label()),
                    Some("worker_pool".to_string()),
                    None,
                    format!("{}: {}", event.worker, event.detail),
                );
            }
        }
    }

    /// 终止旧子进程并解决所有 pending（不改变状态；由调用方设置状态与事件）。
    /// stdin/stdout 生命周期：先取走 stdin 并丢弃（子进程侧读到 EOF 走协议退出），
    /// 再 kill 兜底回收，最后 wait 收尸，保证无孤儿。
    async fn terminate(&self, id: &str, detail: String) {
        let (child, stdin) = {
            let mut inner = self.inner.lock().await;
            // 租约释放：worker 终止（kill/重启/崩溃自愈）即释放租约，迁移语义。
            if let Some(leases) = &inner.leases {
                if let Some(lease) = leases.lease(id) {
                    let _ = leases.release(id, &lease.token);
                }
            }
            if let Some(slot) = inner.workers.get_mut(id) {
                slot.exited = true;
                // 丢弃 ping 期望：退出时心跳必须失败（不得误报 pong 成功）。
                slot.ping.take();
                for (_, tx) in slot.pending.drain() {
                    let _ = tx.send(Err(detail.clone()));
                }
                (slot.child.take(), slot.stdin.take())
            } else {
                (None, None)
            }
        };
        // 先关闭 stdin（优雅路径：子进程协议在 EOF 时正常退出），再 kill 兜底。
        drop(stdin);
        if let Some(mut child) = child {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }

    /// 崩溃自愈：终止 → 指数退避 → 重启；重启即崩则继续退避；连续失败超限 → 熔断。
    async fn handle_crash(&self, id: &str, reason: &str) -> Result<WorkerStatus, PoolError> {
        loop {
            self.terminate(id, format!("崩溃：{reason}")).await;
            let state = {
                let mut inner = self.inner.lock().await;
                let slot = inner
                    .workers
                    .get_mut(id)
                    .ok_or_else(|| PoolError::UnknownWorker(id.to_string()))?;
                slot.supervisor.on_crash()
            };
            match state {
                SupervisionState::Restarting {
                    attempts,
                    next_retry_secs,
                } => {
                    self.emit(
                        id,
                        WorkerEventKind::Crashed,
                        format!("{reason}（attempt {attempts}）"),
                        new_correlation_id(),
                    )
                    .await;
                    if next_retry_secs > 0 {
                        tokio::time::sleep(Duration::from_secs(next_retry_secs)).await;
                    }
                    match self.respawn(id).await {
                        Ok(()) => {
                            self.emit(
                                id,
                                WorkerEventKind::Restarted,
                                format!("attempt {attempts}，退避 {next_retry_secs}s 后重启"),
                                new_correlation_id(),
                            )
                            .await;
                            return Ok(WorkerStatus::Running);
                        }
                        Err(_) => continue, // 重启即崩 → 继续退避/熔断
                    }
                }
                SupervisionState::Fused { attempts } => {
                    {
                        let mut inner = self.inner.lock().await;
                        if let Some(slot) = inner.workers.get_mut(id) {
                            slot.status = WorkerStatus::Fused { attempts };
                        }
                    }
                    self.emit(
                        id,
                        WorkerEventKind::Fused,
                        format!("连续 {attempts} 次失败，熔断"),
                        new_correlation_id(),
                    )
                    .await;
                    return Err(PoolError::Fused(id.to_string()));
                }
                SupervisionState::Healthy => return Ok(WorkerStatus::Running),
            }
        }
    }

    /// 预算中止：kill 子进程 + 解决 pending + 事件 + 状态 Stopped。
    async fn abort_budget(&self, id: &str, reason: &str, correlation_id: &CorrelationId) {
        self.terminate(id, reason.to_string()).await;
        {
            let mut inner = self.inner.lock().await;
            if let Some(slot) = inner.workers.get_mut(id) {
                slot.status = WorkerStatus::Stopped;
            }
        }
        self.emit(
            id,
            WorkerEventKind::BudgetAborted,
            reason.to_string(),
            correlation_id.clone(),
        )
        .await;
    }

    async fn mark_healthy(&self, id: &str) {
        let mut inner = self.inner.lock().await;
        if let Some(slot) = inner.workers.get_mut(id) {
            slot.supervisor.mark_healthy();
        }
    }

    async fn respawn(&self, id: &str) -> Result<(), PoolError> {
        let spec = {
            let inner = self.inner.lock().await;
            inner
                .workers
                .get(id)
                .ok_or_else(|| PoolError::UnknownWorker(id.to_string()))?
                .spec
                .clone()
        };
        self.spawn_inner(spec, id.to_string(), false).await
    }

    /// 启动子进程 + ready 握手。`reset_supervisor=true` 时为全新 worker（重置崩溃计数）。
    async fn spawn_inner(
        &self,
        spec: WorkerSpec,
        id: WorkerId,
        reset_supervisor: bool,
    ) -> Result<(), PoolError> {
        // 若已存在存活子进程（重复 spawn 同 id），先终止。
        self.terminate(&id, "重新 spawn".to_string()).await;

        let mut cmd = tokio::process::Command::new(&spec.command);
        cmd.args(&spec.args);
        if let Some(cwd) = &spec.cwd {
            cmd.current_dir(cwd);
        }
        for (key, value) in &spec.env_whitelist {
            cmd.env(key, value);
        }
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit());
        let mut child = cmd
            .spawn()
            .map_err(|e| PoolError::Spawn(format!("{}: {e}", spec.command.display())))?;
        let pid = child.id();
        // 管道取用失败必须 kill 已 spawn 的进程，不得泄漏孤儿。
        let stdin = match child.stdin.take() {
            Some(stdin) => stdin,
            None => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(PoolError::Spawn("stdin 不可用".to_string()));
            }
        };
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                drop(stdin);
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(PoolError::Spawn("stdout 不可用".to_string()));
            }
        };

        let gen = {
            let mut inner = self.inner.lock().await;
            let slot = inner
                .workers
                .entry(id.clone())
                .or_insert_with(|| WorkerSlot {
                    spec: spec.clone(),
                    child: None,
                    stdin: None,
                    pid: None,
                    supervisor: Supervisor::new(spec.restart_rule),
                    status: WorkerStatus::Starting,
                    turns: 0,
                    started_at: None,
                    exited: false,
                    gen: 0,
                    pending: HashMap::new(),
                    ping: None,
                    bad_lines: 0,
                    last_bad_line_at: None,
                });
            if reset_supervisor {
                slot.supervisor = Supervisor::new(spec.restart_rule);
            }
            slot.gen = slot.gen.saturating_add(1);
            slot.child = Some(child);
            slot.stdin = Some(stdin);
            slot.pid = pid;
            slot.status = WorkerStatus::Starting;
            slot.exited = false;
            slot.turns = 0;
            slot.started_at = None;
            let gen = slot.gen;
            Self::spawn_reader(id.clone(), gen, stdout, inner.tx.clone());
            gen
        };

        // ready 握手轮询。dispatch 按序处理 Ready（status=Running）与 Exited（exited=true），
        // 因此先查 Running 再查 exited：瞬时退出（ready 后立即 exit）的 child 不算启动失败。
        // 过期代（旧 child）的 Exited 由 gen 过滤，不会污染本代状态。
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            {
                let inner = self.inner.lock().await;
                let slot = inner
                    .workers
                    .get(&id)
                    .ok_or_else(|| PoolError::UnknownWorker(id.clone()))?;
                if slot.status == WorkerStatus::Running && slot.gen == gen {
                    break;
                }
                if slot.exited {
                    return Err(PoolError::Spawn(format!(
                        "worker {id} 启动后立即退出（协议未就绪）"
                    )));
                }
            }
            if Instant::now() >= deadline {
                // ready 超时：子进程可能仍存活，必须终止回收，不得泄漏孤儿。
                self.terminate(&id, "ready 超时".to_string()).await;
                return Err(PoolError::Spawn(format!("worker {id} ready 超时")));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        // worker 租约：spawn/重启成功即持有（submit 前 fencing 校验；终止时释放）。
        {
            let inner = self.inner.lock().await;
            if let Some(leases) = &inner.leases {
                if let Err(e) = leases.acquire(&id) {
                    self.terminate(&id, "租约获取失败".to_string()).await;
                    return Err(PoolError::Spawn(format!("worker {id} 租约获取失败：{e}")));
                }
            }
        }
        Ok(())
    }

    /// 子进程 stdout 读取任务：逐行解析结构化消息送入调度循环；EOF 上报 Exited。
    /// 消息携带 spawn 代数（gen），调度循环据此丢弃过期代（旧 child）的消息。
    fn spawn_reader(
        worker: WorkerId,
        gen: u64,
        stdout: ChildStdout,
        tx: mpsc::UnboundedSender<ChildOutcome>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let outcome = match parse_child_line(&line) {
                            Ok(msg) => outcome_from_msg(&worker, gen, msg),
                            Err(_) => ChildOutcome::BadLine {
                                worker: worker.clone(),
                                gen,
                            },
                        };
                        if tx.send(outcome).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            let _ = tx.send(ChildOutcome::Exited { worker, gen });
        })
    }

    /// 调度循环：独占处理子进程回报（ready/pong/result/exit/badline）。
    async fn dispatch_loop(
        inner: Arc<AsyncMutex<PoolInner>>,
        mut rx: mpsc::UnboundedReceiver<ChildOutcome>,
    ) {
        while let Some(outcome) = rx.recv().await {
            let mut inner = inner.lock().await;
            match outcome {
                ChildOutcome::Ready { worker, gen } => {
                    if let Some(slot) = inner.workers.get_mut(&worker) {
                        if slot.gen != gen {
                            continue; // 过期代消息（旧 child 的 ready），忽略
                        }
                        if matches!(
                            slot.status,
                            WorkerStatus::Starting | WorkerStatus::Restarting { .. }
                        ) {
                            slot.status = WorkerStatus::Running;
                            slot.exited = false;
                            slot.started_at = Some(Instant::now());
                        }
                    }
                }
                ChildOutcome::Pong { worker, gen } => {
                    if let Some(slot) = inner.workers.get_mut(&worker) {
                        if slot.gen != gen {
                            continue;
                        }
                        if let Some(tx) = slot.ping.take() {
                            let _ = tx.send(());
                        }
                    }
                }
                ChildOutcome::Result {
                    worker,
                    gen,
                    task_id,
                    result,
                } => {
                    if let Some(slot) = inner.workers.get_mut(&worker) {
                        // 旧代结果的 task_id 与当代 UUID 不冲突，但按代过滤更严谨。
                        if slot.gen != gen {
                            continue;
                        }
                        if let Some(tx) = slot.pending.remove(&task_id) {
                            let _ = tx.send(result);
                        }
                    }
                }
                ChildOutcome::Exited { worker, gen } => {
                    if let Some(slot) = inner.workers.get_mut(&worker) {
                        if slot.gen != gen {
                            continue;
                        }
                        slot.exited = true;
                        for (_, tx) in slot.pending.drain() {
                            let _ = tx.send(Err("worker 已退出".to_string()));
                        }
                        // 丢弃 ping 期望：退出时心跳必须失败（不得误报 pong 成功）。
                        slot.ping.take();
                    }
                }
                // 非结构化行：忽略（不解释、不传递），协议严格性体现在"只认 JSON 行"。
                ChildOutcome::BadLine { worker, gen } => {
                    if let Some(slot) = inner.workers.get_mut(&worker) {
                        if slot.gen != gen {
                            continue;
                        }
                        slot.bad_lines += 1;
                        slot.last_bad_line_at = Some(std::time::Instant::now());
                    }
                }
            }
        }
    }
}

/// 池 worker 适配：让 `goal.rs` 的 `Worker` trait 走子进程执行。
pub struct PoolWorker {
    pool: WorkerPool,
    worker_id: WorkerId,
}

impl fmt::Debug for PoolWorker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PoolWorker")
            .field("worker_id", &self.worker_id)
            .finish()
    }
}

impl PoolWorker {
    pub fn new(pool: WorkerPool, worker_id: impl Into<WorkerId>) -> Self {
        Self {
            pool,
            worker_id: worker_id.into(),
        }
    }
}

#[async_trait]
impl crate::goal::Worker for PoolWorker {
    fn name(&self) -> &str {
        &self.worker_id
    }

    async fn run(&self, input: &serde_json::Value) -> Result<String, String> {
        self.pool
            .submit(&self.worker_id, input)
            .await
            .map_err(|e| e.to_string())
    }
}

/// 子进程侧协议入口（测试二进制 / 真实 worker 宿主均可复用）。
pub mod child {
    use serde_json::Value;
    use std::io::{BufRead, Write};

    /// 子进程协议主循环：启动即上报 `ready`，然后从 stdin 读 JSON 行、向 stdout 写 JSON 行
    /// （stderr 供人读诊断）。
    /// - `task`：调用 `handler(input)`，结果回写 `result`（ok/error 字段）。
    /// - `ping`：回 `pong`。
    /// - `shutdown` / EOF：正常退出（exit 0）。
    /// - handler 内可用 `std::process::exit(n)` 模拟崩溃。
    pub fn run_child_protocol<F>(mut handler: F) -> !
    where
        F: FnMut(&Value) -> Result<String, String>,
    {
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        let mut out = std::io::BufWriter::new(stdout.lock());
        // ready 握手：告知父进程结构化协议已经就绪
        let _ = writeln!(out, "{{\"type\":\"ready\"}}");
        let _ = out.flush();
        let mut line = String::new();
        loop {
            line.clear();
            let read = match stdin.lock().read_line(&mut line) {
                Ok(n) => n,
                Err(_) => break,
            };
            if read == 0 {
                break;
            }
            let msg: Value = match serde_json::from_str(line.trim()) {
                Ok(v) => v,
                Err(_) => continue,
            };
            match msg.get("cmd").and_then(Value::as_str) {
                Some("shutdown") => break,
                Some("ping") => {
                    let _ = writeln!(out, "{{\"type\":\"pong\"}}");
                }
                Some("cancel") => {}
                Some("task") => {
                    let task_id = msg
                        .get("task_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let input = msg.get("input").cloned().unwrap_or(Value::Null);
                    match handler(&input) {
                        Ok(output) => {
                            let _ = writeln!(
                                out,
                                "{{\"type\":\"result\",\"task_id\":{},\"ok\":true,\"output\":{}}}",
                                serde_json::json!(task_id),
                                serde_json::json!(output)
                            );
                        }
                        Err(error) => {
                            let _ = writeln!(
                                out,
                                "{{\"type\":\"result\",\"task_id\":{},\"ok\":false,\"error\":{}}}",
                                serde_json::json!(task_id),
                                serde_json::json!(error)
                            );
                        }
                    }
                }
                _ => {}
            }
            let _ = out.flush();
        }
        std::process::exit(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_child_line_accepts_structured_messages() {
        assert!(matches!(
            parse_child_line("{\"type\":\"ready\"}").unwrap(),
            ChildMsg::Ready
        ));
        assert!(matches!(
            parse_child_line("{\"type\":\"pong\"}").unwrap(),
            ChildMsg::Pong
        ));
        match parse_child_line(
            "{\"type\":\"result\",\"task_id\":\"t1\",\"ok\":true,\"output\":\"out-A\"}",
        )
        .unwrap()
        {
            ChildMsg::Result {
                task_id,
                ok,
                output,
                ..
            } => {
                assert_eq!(task_id, "t1");
                assert!(ok);
                assert_eq!(output.as_deref(), Some("out-A"));
            }
            _ => panic!("应解析为 result"),
        }
    }

    #[test]
    fn parse_child_line_rejects_free_text() {
        assert!(parse_child_line("hello world").is_err());
        assert!(parse_child_line("just some log").is_err());
        assert!(parse_child_line("").is_err());
    }

    #[test]
    fn parent_lines_are_structured_json() {
        let input = serde_json::json!({ "text": "A" });
        let line = task_line("t1", "corr-1", &input);
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["cmd"], "task");
        assert_eq!(parsed["task_id"], "t1");
        assert_eq!(parsed["correlation_id"], "corr-1");
        assert_eq!(parsed["input"], input);
        let ping: serde_json::Value = serde_json::from_str(&ping_line()).unwrap();
        assert_eq!(ping["cmd"], "ping");
        let cancel: serde_json::Value = serde_json::from_str(&cancel_line("t9")).unwrap();
        assert_eq!(cancel["cmd"], "cancel");
        assert_eq!(cancel["task_id"], "t9");
        let shutdown: serde_json::Value = serde_json::from_str(&shutdown_line()).unwrap();
        assert_eq!(shutdown["cmd"], "shutdown");
    }

    #[test]
    fn worker_spec_serde_roundtrip() {
        let spec = WorkerSpec::new("w1", "worker.exe")
            .args(vec!["-x".to_string()])
            .env_whitelist(vec![("K".to_string(), "V".to_string())])
            .budget(WorkerBudget {
                max_turns: 5,
                max_duration_secs: 60,
                max_memory_mb: 256,
                max_cpu_cores: 2.0,
            });
        let json = serde_json::to_string(&spec).unwrap();
        let restored: WorkerSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.id, "w1");
        assert_eq!(restored.args, vec!["-x".to_string()]);
        assert_eq!(restored.budget.max_turns, 5);
        assert_eq!(restored.budget.max_cpu_cores, 2.0);
        assert_eq!(restored.isolation, IsolationMode::Process);
    }

    #[test]
    fn worker_budget_exceeded_semantics() {
        let budget = WorkerBudget {
            max_turns: 3,
            max_duration_secs: 10,
            ..Default::default()
        };
        assert!(!budget.exceeded(2, Duration::from_secs(5)));
        assert!(budget.exceeded(3, Duration::from_secs(5)));
        assert!(budget.exceeded(2, Duration::from_secs(10)));
        let unlimited = WorkerBudget::default();
        assert!(!unlimited.exceeded(999, Duration::from_secs(99999)));
    }

    #[test]
    fn isolation_default_is_process() {
        assert_eq!(IsolationMode::default(), IsolationMode::Process);
        let spec = WorkerSpec::new("w", "x.exe");
        assert_eq!(spec.isolation, IsolationMode::Process);
    }

    #[test]
    fn worker_status_display() {
        assert_eq!(WorkerStatus::Running.to_string(), "running");
        assert_eq!(
            WorkerStatus::Restarting {
                attempts: 2,
                next_retry_secs: 4
            }
            .to_string(),
            "restarting(attempt=2, backoff=4s)"
        );
        assert_eq!(
            WorkerStatus::Fused { attempts: 4 }.to_string(),
            "fused(attempt=4)"
        );
        assert_eq!(WorkerStatus::Stopped.to_string(), "stopped");
    }
}
