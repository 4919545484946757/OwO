// R12:node_agent 完成，待主控接线
//! 节点 agent：心跳、CapabilityCard 自报、本地 supervisor、控制面注册/心跳续租/离线恢复与状态持久化。
//!
//! 设计来源：《多Agent并行体系-生产级设计与跨机扩展-2026-08-16.md》§3/§4：
//! - **心跳**：`heartbeat()` 刷新活跃时间；心跳超时走本地 [`crate::fleet::Supervisor`]
//!   崩溃计数（退避/熔断），并联动能力注册表健康度（`CapabilityWorkerRegistry::mark_health`）。
//! - **能力自报**：节点启动以 CapabilityCard 注册到本地能力注册表（跨机路由铺路）。
//! - **本地监督**：`SupervisionState` 直接复用 `fleet::Supervisor`（退避 + 熔断语义一致）。
//! - **控制面（R12）**：`register_with_control_plane` 向控制面注册并持租约（心跳续租，
//!   fencing 由 [`crate::lease::LeaseManager`] 统一）；心跳/离线/恢复事件经
//!   [`crate::fleet::AgentBus`] 落盘到 [`crate::bus_store::BusStore`]；
//!   状态变迁记入 [`crate::experience_store::ExperienceStore`]（节点注册/离线/恢复幂等键）。

use crate::capability::{CapabilityCard, CapabilityWorkerRegistry};
use crate::experience_store::{Attribution, ExperienceStore, Outcome};
use crate::fleet::{
    AgentBus, RestartRule, SupervisionState, Supervisor, WorkerEvent, WorkerEventKind,
};
use crate::lease::{Lease, LeaseManager};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 节点心跳超时（默认 3s 无心跳视为失联）。
const DEFAULT_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(3);

/// 心跳事件落盘降频（每 N 次心跳发一次 Healthy 总线事件，避免日志爆炸）。
const HEARTBEAT_PERSIST_EVERY: u64 = 5;

/// 节点 agent。
#[derive(Debug, Clone)]
pub struct NodeAgent {
    id: String,
    card: CapabilityCard,
    /// 本地 supervisor（&self 方法内变更崩溃计数）。
    supervisor: Arc<std::sync::Mutex<Supervisor>>,
    /// 最近心跳时刻。
    last_heartbeat: Arc<std::sync::Mutex<Option<Instant>>>,
    /// 心跳计数。
    heartbeat_count: Arc<AtomicU64>,
    /// 失联标记。
    lost: Arc<AtomicBool>,
    heartbeat_timeout: Duration,
    /// 控制面租约（R12：节点注册持租约，心跳续租）。
    leases: Arc<std::sync::Mutex<Option<LeaseManager>>>,
    /// 当前租约 token（续租/fencing 校验用）。
    lease_token: Arc<std::sync::Mutex<Option<String>>>,
    /// 总线持久化（R12：心跳/离线/恢复事件落盘）。
    bus: Arc<std::sync::Mutex<Option<AgentBus>>>,
    /// 经验库（R12：节点状态变迁记录）。
    experience: Arc<std::sync::Mutex<Option<ExperienceStore>>>,
}

impl NodeAgent {
    pub fn new(id: impl Into<String>, card: CapabilityCard) -> Self {
        Self::with_timeout(id, card, DEFAULT_HEARTBEAT_TIMEOUT, RestartRule::default())
    }

    pub fn with_timeout(
        id: impl Into<String>,
        card: CapabilityCard,
        heartbeat_timeout: Duration,
        restart_rule: RestartRule,
    ) -> Self {
        Self {
            id: id.into(),
            card,
            supervisor: Arc::new(std::sync::Mutex::new(Supervisor::new(restart_rule))),
            last_heartbeat: Arc::new(std::sync::Mutex::new(Some(Instant::now()))),
            heartbeat_count: Arc::new(AtomicU64::new(0)),
            lost: Arc::new(AtomicBool::new(false)),
            heartbeat_timeout,
            leases: Arc::new(std::sync::Mutex::new(None)),
            lease_token: Arc::new(std::sync::Mutex::new(None)),
            bus: Arc::new(std::sync::Mutex::new(None)),
            experience: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// 挂接控制面持久化依赖（租约/总线/经验库；幂等）。
    pub fn attach_control_plane(
        &self,
        leases: LeaseManager,
        bus: AgentBus,
        experience: ExperienceStore,
    ) {
        if let Ok(mut l) = self.leases.lock() {
            *l = Some(leases);
        }
        if let Ok(mut b) = self.bus.lock() {
            *b = Some(bus);
        }
        if let Ok(mut e) = self.experience.lock() {
            *e = Some(experience);
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// 能力自报：注册到本地能力注册表（跨机路由随注册表分发）。
    pub fn register_to(&self, registry: &CapabilityWorkerRegistry) {
        registry.register(self.card.clone());
    }

    pub fn report_capabilities(&self) -> &CapabilityCard {
        &self.card
    }

    /// 当前租约 token（诊断/续租）。
    pub fn lease_token(&self) -> Option<String> {
        self.lease_token.lock().ok().and_then(|t| t.clone())
    }

    /// 心跳：刷新活跃时间并计数。
    pub fn heartbeat(&self) {
        if let Ok(mut last) = self.last_heartbeat.lock() {
            *last = Some(Instant::now());
        }
        self.heartbeat_count.fetch_add(1, Ordering::SeqCst);
        self.lost.store(false, Ordering::SeqCst);
    }

    /// 心跳 + 向能力注册表上报健康（worker 生命周期事件接线）。
    pub fn heartbeat_and_report(&self, registry: &CapabilityWorkerRegistry) {
        self.heartbeat();
        registry.mark_health(&self.id, true);
    }

    /// 控制面注册：持租约 + 能力注册 + 注册事件落盘 + 经验记录。返回租约。
    pub async fn register_with_control_plane(
        &self,
        registry: &CapabilityWorkerRegistry,
    ) -> Result<Lease, String> {
        let lease = {
            let leases = self.leases.lock().ok().and_then(|l| l.clone());
            match leases {
                Some(leases) => {
                    let lease = leases
                        .acquire(&self.id)
                        .map_err(|e| format!("节点租约获取失败：{e}"))?;
                    if let Ok(mut t) = self.lease_token.lock() {
                        *t = Some(lease.token.clone());
                    }
                    lease
                }
                None => Lease {
                    holder: self.id.clone(),
                    token: String::new(),
                    epoch: 0,
                    ttl: Duration::from_secs(0),
                    expires_at_unix_ms: 0,
                    renewed_at_unix_ms: 0,
                },
            }
        };
        self.register_to(registry);
        registry.mark_health(&self.id, true);
        self.emit_event(WorkerEventKind::Started, "节点注册").await;
        self.record_experience(
            format!("node:register:{}", self.id),
            Outcome::Success,
            Some("节点注册"),
        );
        Ok(lease)
    }

    /// 心跳 + 续租 + 健康上报 + 降频事件落盘。返回租约是否仍有效（续租失败 → 重新获取）。
    pub async fn heartbeat_and_report_persisted(
        &self,
        registry: &CapabilityWorkerRegistry,
    ) -> bool {
        self.heartbeat_and_report(registry);
        let leases = self.leases.lock().ok().and_then(|l| l.clone());
        let Some(leases) = leases else {
            return false;
        };
        let token = self.lease_token();
        let valid = match token {
            Some(token) => leases.renew(&self.id, &token).is_ok(),
            None => false,
        };
        if !valid {
            // 租约被 fencing 迁移（过期/分区）：重连重新获取，旧 token 作废。
            if let Ok(lease) = leases.acquire(&self.id) {
                if let Ok(mut t) = self.lease_token.lock() {
                    *t = Some(lease.token.clone());
                }
            }
        }
        let count = self.heartbeat_count();
        if count.is_multiple_of(HEARTBEAT_PERSIST_EVERY) {
            self.emit_event(WorkerEventKind::Started, "心跳健康").await;
        }
        valid
    }

    /// 失联检测：距最近心跳超过超时 → 上报本地 supervisor（崩溃计数/退避/熔断）。
    pub fn check_liveness(&self) -> SupervisionState {
        let stale = self
            .last_heartbeat
            .lock()
            .map(|l| {
                l.map(|t| t.elapsed() >= self.heartbeat_timeout)
                    .unwrap_or(true)
            })
            .unwrap_or(true);
        if !stale {
            self.lost.store(false, Ordering::SeqCst);
            return SupervisionState::Healthy;
        }
        self.lost.store(true, Ordering::SeqCst);
        self.supervisor
            .lock()
            .map(|mut s| s.on_crash())
            .unwrap_or(SupervisionState::Healthy)
    }

    /// 失联/熔断上报控制面：离线事件落盘 + 经验记录（幂等）。
    pub async fn mark_offline(&self, state: &SupervisionState) {
        self.lost.store(true, Ordering::SeqCst);
        let kind = match state {
            SupervisionState::Fused { .. } => WorkerEventKind::Fused,
            _ => WorkerEventKind::Crashed,
        };
        let detail = match state {
            SupervisionState::Fused { attempts } => format!("节点失联熔断（{attempts} 次）"),
            SupervisionState::Restarting { attempts, .. } => {
                format!("节点失联（{attempts} 次）")
            }
            SupervisionState::Healthy => "节点失联".to_string(),
        };
        self.emit_event(kind, detail).await;
        self.record_experience(
            format!("node:offline:{}", self.id),
            Outcome::Failure,
            Some("节点离线"),
        );
    }

    /// 失联后重连：复位心跳与本地 supervisor 崩溃计数（重连即恢复），
    /// 上报控制面恢复事件 + 经验记录（幂等）。返回重连前是否已熔断。
    pub async fn reconnect_persisted(&self, registry: &CapabilityWorkerRegistry) -> bool {
        let was_fused = self.reconnect();
        registry.mark_health(&self.id, true);
        self.emit_event(WorkerEventKind::Restarted, "节点重连恢复")
            .await;
        self.record_experience(
            format!("node:recover:{}", self.id),
            Outcome::Success,
            Some("节点恢复"),
        );
        was_fused
    }

    /// 失联后重连：复位计数（不落盘版本）。
    pub fn reconnect(&self) -> bool {
        self.heartbeat();
        let was_fused = self
            .supervisor
            .lock()
            .map(|s| s.attempts() > s.rule().max_restarts)
            .unwrap_or(false);
        if let Ok(mut s) = self.supervisor.lock() {
            s.mark_healthy();
        }
        self.lost.store(false, Ordering::SeqCst);
        was_fused
    }

    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }

    pub fn heartbeat_count(&self) -> u64 {
        self.heartbeat_count.load(Ordering::SeqCst)
    }

    pub fn restarts(&self) -> u32 {
        self.supervisor.lock().map(|s| s.attempts()).unwrap_or(0)
    }

    /// 节点生命周期事件落盘（总线 → bus_store）。
    async fn emit_event(&self, kind: WorkerEventKind, detail: impl Into<String>) {
        let bus = self.bus.lock().ok().and_then(|b| b.clone());
        let Some(bus) = bus else {
            return;
        };
        let event = WorkerEvent::new(self.id.clone(), kind, detail, format!("node:{}", self.id));
        // 目标为控制面 agent（若未注册则静默跳过——持久化由 bus_store 独立承载）。
        let _ = bus
            .send_worker_event(self.id.clone(), crate::fleet::CONTROL_PLANE_AGENT, &event)
            .await;
    }

    /// 节点状态变迁经验记录（幂等键 correlation_id）。
    fn record_experience(&self, correlation_id: String, outcome: Outcome, detail: Option<&str>) {
        let experience = self.experience.lock().ok().and_then(|e| e.clone());
        let Some(experience) = experience else {
            return;
        };
        let _ = experience.record_worker_outcome(
            correlation_id,
            self.id.clone(),
            outcome,
            Attribution {
                goal_id: None,
                plan_id: None,
                step_id: None,
                input_keys: Vec::new(),
                error: detail.map(|s| s.to_string()),
            },
        );
    }
}

/// 节点运行时状态快照（诊断/观测用）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NodeStatus {
    pub id: String,
    pub healthy: bool,
    pub lost: bool,
    pub heartbeats: u64,
    pub restarts: u32,
    #[serde(default)]
    pub registered: bool,
    #[serde(default)]
    pub lease_epoch: Option<u64>,
    pub card: CapabilityCard,
}

impl NodeAgent {
    pub fn status(&self) -> NodeStatus {
        let registered = self.leases.lock().ok().and_then(|l| l.clone()).is_some();
        let lease_epoch = self
            .leases
            .lock()
            .ok()
            .and_then(|l| l.clone())
            .and_then(|leases| leases.lease(&self.id).map(|l| l.epoch));
        NodeStatus {
            id: self.id.clone(),
            healthy: !self.is_lost(),
            lost: self.is_lost(),
            heartbeats: self.heartbeat_count(),
            restarts: self.restarts(),
            registered,
            lease_epoch,
            card: self.card.clone(),
        }
    }
}
