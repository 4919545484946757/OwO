// R10:capability 完成（批量注册接入），待主控接线
//! CapabilityCard 与本地能力路由（多 Agent 编排，跨机路由铺路）。
//!
//! 设计来源：《多Agent并行体系-生产级设计与跨机扩展-2026-08-16.md》§3 能力卡：
//! - [`CapabilityCard`]：worker 的静态能力声明（os/arch/actions/perception/models/
//!   resources/trust/egress）；未来跨机路由时随注册表分发。
//! - [`WorkerRequirement`]：任务对 worker 的能力需求（子集语义：未声明维度不约束）。
//! - [`CapabilityWorkerRegistry`]：本地注册表 + 按能力匹配路由；
//!   需求不满足时明确返回 [`RouteDecision::Degrade`] / [`RouteDecision::Reject`]，
//!   不做静默回退。
//!   R9：注册表支持 JSON 快照持久化（[`CapabilityWorkerRegistry::persist`/`load`）、
//!   worker 健康度（[`CapabilityWorkerRegistry::mark_health`，路由跳过近期失败过多的 worker）、
//!   路由命中率统计（[`CapabilityWorkerRegistry::route_stats`]）。
//!   只做本地语义（进程内 HashMap），不接网络。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// 操作系统。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Os {
    Windows,
    Linux,
    MacOs,
    Other,
}

impl Os {
    pub fn current() -> Self {
        #[cfg(target_os = "windows")]
        {
            Self::Windows
        }
        #[cfg(target_os = "linux")]
        {
            Self::Linux
        }
        #[cfg(target_os = "macos")]
        {
            Self::MacOs
        }
        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        {
            Self::Other
        }
    }
}

/// CPU 架构。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Arch {
    X86_64,
    Aarch64,
    Other,
}

impl Arch {
    pub fn current() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            Self::X86_64
        }
        #[cfg(target_arch = "aarch64")]
        {
            Self::Aarch64
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            Self::Other
        }
    }
}

/// 信任等级（能力卡声明；需求方可设最低信任线）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    /// 本地受信（本机安装、可完全访问）。
    #[default]
    Local,
    /// 半受信（隔离受限，如子进程池进程隔离）。
    SemiTrusted,
    /// 不可信（第三方/网络来源，必须沙箱）。
    Untrusted,
}

/// 出网模式。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EgressMode {
    /// 禁止出网。
    #[default]
    None,
    /// 仅允许名单内的主机。
    AllowList(Vec<String>),
    /// 开放出网。
    Open,
}

/// 资源规格（策略字段；OS 强制由沙箱实现）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resources {
    pub cpu_cores: u32,
    pub memory_mb: u64,
    pub disk_gb: u64,
}

impl Default for Resources {
    fn default() -> Self {
        Self {
            cpu_cores: 1,
            memory_mb: 512,
            disk_gb: 10,
        }
    }
}

/// worker 能力卡：静态声明，随注册表路由。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityCard {
    /// 注册名（与 pool / WorkerRegistry 的 worker id 对齐）。
    pub worker: String,
    pub os: Os,
    pub arch: Arch,
    /// 可执行动作集合（如 ["shell", "file_write", "browser"]）。
    #[serde(default)]
    pub actions: Vec<String>,
    /// 可感知模态（如 ["screen", "ocr", "stt", "a11y"]）。
    #[serde(default)]
    pub perception: Vec<String>,
    /// 可用模型（如 ["local:qwen2.5", "cloud:gpt-4o"]）。
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub resources: Resources,
    #[serde(default)]
    pub trust: TrustLevel,
    #[serde(default)]
    pub egress: EgressMode,
}

impl CapabilityCard {
    pub fn new(worker: impl Into<String>) -> Self {
        Self {
            worker: worker.into(),
            os: Os::current(),
            arch: Arch::current(),
            actions: Vec::new(),
            perception: Vec::new(),
            models: Vec::new(),
            resources: Resources::default(),
            trust: TrustLevel::SemiTrusted,
            egress: EgressMode::None,
        }
    }

    pub fn actions(mut self, actions: Vec<String>) -> Self {
        self.actions = actions;
        self
    }

    pub fn models(mut self, models: Vec<String>) -> Self {
        self.models = models;
        self
    }

    pub fn trust(mut self, trust: TrustLevel) -> Self {
        self.trust = trust;
        self
    }
}

/// 任务能力需求：未声明的维度不约束（子集语义）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerRequirement {
    pub os: Option<Os>,
    pub arch: Option<Arch>,
    /// 至少需要这些动作（空 = 不约束）。
    #[serde(default)]
    pub actions: Vec<String>,
    /// 至少需要这些感知模态（空 = 不约束）。
    #[serde(default)]
    pub perception: Vec<String>,
    /// 至少需要这些模型之一（空 = 不约束）。
    #[serde(default)]
    pub models: Vec<String>,
    pub min_resources: Option<Resources>,
    pub min_trust: Option<TrustLevel>,
    pub egress: Option<EgressMode>,
}

/// 能力匹配结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityMatch {
    /// 完全满足。
    Full,
    /// 可降级执行（缺失部分非硬约束维度）。
    Partial { missing: Vec<String> },
    /// 不满足（硬约束缺失），拒绝。
    Unfit { reasons: Vec<String> },
}

fn contains_any(haystack: &[String], needles: &[String]) -> bool {
    needles.iter().any(|n| haystack.iter().any(|h| h == n))
}

/// 逐项评估能力卡对需求的匹配度。
/// （命名避开 `sandbox::evaluate_capability`，语义不同：本函数是能力卡匹配评估。）
pub fn evaluate_capability_match(
    card: &CapabilityCard,
    req: &WorkerRequirement,
) -> CapabilityMatch {
    let mut missing: Vec<String> = Vec::new();
    let mut reasons: Vec<String> = Vec::new();
    if let Some(os) = req.os {
        if card.os != os {
            reasons.push(format!("os 不满足：需要 {os:?}，实际 {:?}", card.os));
        }
    }
    if let Some(arch) = req.arch {
        if card.arch != arch {
            reasons.push(format!("arch 不满足：需要 {arch:?}，实际 {:?}", card.arch));
        }
    }
    if !req.actions.is_empty() {
        for a in &req.actions {
            if !card.actions.iter().any(|h| h == a) {
                missing.push(format!("action:{a}"));
            }
        }
    }
    if !req.perception.is_empty() {
        for p in &req.perception {
            if !card.perception.iter().any(|h| h == p) {
                missing.push(format!("perception:{p}"));
            }
        }
    }
    if !req.models.is_empty() && !contains_any(&card.models, &req.models) {
        reasons.push(format!(
            "models 不满足：需要 {} 之一，实际 {:?}",
            req.models.join("/"),
            card.models
        ));
    }
    if let Some(r) = req.min_resources {
        if card.resources.cpu_cores < r.cpu_cores {
            reasons.push(format!(
                "cpu 不足：需要 {} 核，实际 {}",
                r.cpu_cores, card.resources.cpu_cores
            ));
        }
        if card.resources.memory_mb < r.memory_mb {
            reasons.push(format!(
                "内存不足：需要 {}MB，实际 {}MB",
                r.memory_mb, card.resources.memory_mb
            ));
        }
        if card.resources.disk_gb < r.disk_gb {
            reasons.push(format!(
                "磁盘不足：需要 {}GB，实际 {}GB",
                r.disk_gb, card.resources.disk_gb
            ));
        }
    }
    if let Some(min) = req.min_trust {
        if card.trust < min {
            reasons.push(format!("信任不足：需要 {min:?}，实际 {:?}", card.trust));
        }
    }
    if let Some(mode) = &req.egress {
        let ok = match (&card.egress, mode) {
            (EgressMode::None, EgressMode::None) => true,
            (EgressMode::Open, _) => true,
            (EgressMode::AllowList(have), EgressMode::AllowList(need)) => {
                need.iter().all(|n| have.iter().any(|h| h == n))
            }
            (EgressMode::AllowList(_), EgressMode::None) => true,
            _ => false,
        };
        if !ok {
            reasons.push(format!(
                "egress 不满足：需要 {mode:?}，实际 {:?}",
                card.egress
            ));
        }
    }
    if !reasons.is_empty() {
        return CapabilityMatch::Unfit { reasons };
    }
    if !missing.is_empty() {
        return CapabilityMatch::Partial { missing };
    }
    CapabilityMatch::Full
}

/// 路由决策：明确降级/拒绝，绝不静默。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteDecision {
    /// 选中 worker。
    Pick(String),
    /// 无完全匹配，但可降级（缺失为可降级维度）。
    Degrade {
        worker: String,
        missing: Vec<String>,
    },
    /// 无可用 worker，附拒绝原因。
    Reject { reasons: Vec<String> },
}

/// worker 健康度（路由决策参考：近期失败过多者被跳过并显式说明）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorkerHealth {
    /// 成功心跳/任务数。
    pub successes: u64,
    /// 失败数（含拒绝/超时）。
    pub failures: u64,
    /// 最后观测时间（epoch 秒；0 = 未知）。
    pub last_seen_secs: u64,
}

impl WorkerHealth {
    pub fn success_rate(&self) -> f64 {
        let total = self.successes + self.failures;
        if total == 0 {
            1.0
        } else {
            self.successes as f64 / total as f64
        }
    }
}

/// 路由命中率统计。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RouteStats {
    pub picks: u64,
    pub degrades: u64,
    pub rejects: u64,
}

impl RouteStats {
    pub fn total(&self) -> u64 {
        self.picks + self.degrades + self.rejects
    }
}

/// 注册表快照（持久化格式：能力卡 + 健康度 + 命中率）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegistrySnapshot {
    pub cards: Vec<CapabilityCard>,
    #[serde(default)]
    pub health: HashMap<String, WorkerHealth>,
    #[serde(default)]
    pub stats: RouteStats,
}

/// 本地能力注册表（进程内；跨机分发留待网络层）。
#[derive(Clone, Default, Debug)]
pub struct CapabilityWorkerRegistry {
    cards: Arc<Mutex<HashMap<String, CapabilityCard>>>,
    health: Arc<Mutex<HashMap<String, WorkerHealth>>>,
    stats: Arc<Mutex<RouteStats>>,
}

impl CapabilityWorkerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, card: CapabilityCard) {
        if let Ok(mut cards) = self.cards.lock() {
            cards.insert(card.worker.clone(), card);
        }
    }

    /// 批量注册（节点能力自报接入点：`NodeAgent::register_to` 的批量形态）。
    pub fn register_many(&self, cards: Vec<CapabilityCard>) {
        for card in cards {
            self.register(card);
        }
    }

    pub fn unregister(&self, worker: &str) -> bool {
        self.cards
            .lock()
            .map(|mut c| c.remove(worker).is_some())
            .unwrap_or(false)
    }

    pub fn card(&self, worker: &str) -> Option<CapabilityCard> {
        self.cards.lock().ok().and_then(|c| c.get(worker).cloned())
    }

    pub fn cards(&self) -> Vec<CapabilityCard> {
        self.cards
            .lock()
            .map(|c| c.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.cards.lock().map(|c| c.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 单 worker 能力评估；未注册返回 None。
    pub fn evaluate_worker(
        &self,
        worker: &str,
        req: &WorkerRequirement,
    ) -> Option<CapabilityMatch> {
        let card = self.card(worker)?;
        Some(evaluate_capability_match(&card, req))
    }

    /// 记录 worker 健康度（心跳/任务成败；路由会跳过近期失败过多的 worker）。
    pub fn mark_health(&self, worker: &str, ok: bool) {
        if let Ok(mut health) = self.health.lock() {
            let entry = health.entry(worker.to_string()).or_default();
            if ok {
                entry.successes = entry.successes.saturating_add(1);
            } else {
                entry.failures = entry.failures.saturating_add(1);
            }
            entry.last_seen_secs = chrono::Utc::now().timestamp().max(0) as u64;
        }
    }

    pub fn worker_health(&self, worker: &str) -> Option<WorkerHealth> {
        self.health.lock().ok().and_then(|h| h.get(worker).copied())
    }

    pub fn health(&self) -> HashMap<String, WorkerHealth> {
        self.health.lock().map(|h| h.clone()).unwrap_or_default()
    }

    /// 路由命中率统计。
    pub fn route_stats(&self) -> RouteStats {
        self.stats.lock().map(|s| *s).unwrap_or_default()
    }

    /// 健康度不足判定：失败数 ≥ 3 且失败 ≥ 成功（近期持续不可用）。
    fn unhealthy(&self, worker: &str) -> bool {
        let Some(h) = self.worker_health(worker) else {
            return false;
        };
        h.failures >= 3 && h.failures >= h.successes
    }

    /// 按能力路由：Full 优先；其次最少缺失的 Partial（确定性：按 worker 名排序取首个）；
    /// 健康度不足的 worker 被跳过并显式计入拒绝原因；否则 Reject。全程记录命中率。
    pub fn route(&self, req: &WorkerRequirement) -> RouteDecision {
        let mut pick: Option<String> = None;
        let mut degrade: Option<(String, Vec<String>)> = None;
        let mut reasons: Vec<String> = Vec::new();
        let mut found_any = false;
        // 确定性路由：HashMap 无序，按 worker 名排序后遍历。
        let mut cards = self.cards();
        cards.sort_by(|a, b| a.worker.cmp(&b.worker));
        for card in cards {
            found_any = true;
            if self.unhealthy(&card.worker) {
                reasons.push(format!("{}: 健康度不足（近期失败过多）", card.worker));
                continue;
            }
            match evaluate_capability_match(&card, req) {
                CapabilityMatch::Full => {
                    pick = Some(card.worker);
                    break;
                }
                CapabilityMatch::Partial { missing } => {
                    if degrade
                        .as_ref()
                        .map(|(_, m)| missing.len() < m.len())
                        .unwrap_or(true)
                    {
                        degrade = Some((card.worker, missing));
                    }
                }
                CapabilityMatch::Unfit { reasons: r } => {
                    reasons.push(format!("{}: {}", card.worker, r.join("；")));
                }
            }
        }
        let decision = if let Some(worker) = pick {
            RouteDecision::Pick(worker)
        } else if let Some((worker, missing)) = degrade {
            RouteDecision::Degrade { worker, missing }
        } else if found_any {
            RouteDecision::Reject { reasons }
        } else {
            RouteDecision::Reject {
                reasons: vec!["能力注册表为空".to_string()],
            }
        };
        if let Ok(mut stats) = self.stats.lock() {
            match &decision {
                RouteDecision::Pick(_) => stats.picks = stats.picks.saturating_add(1),
                RouteDecision::Degrade { .. } => stats.degrades = stats.degrades.saturating_add(1),
                RouteDecision::Reject { .. } => stats.rejects = stats.rejects.saturating_add(1),
            }
        }
        decision
    }

    /// 持久化注册表快照（能力卡 + 健康度 + 命中率）到 JSON 文件。
    pub fn persist(&self, path: &Path) -> Result<(), String> {
        let snapshot = RegistrySnapshot {
            cards: self.cards(),
            health: self.health(),
            stats: self.route_stats(),
        };
        let json = serde_json::to_string_pretty(&snapshot)
            .map_err(|e| format!("能力注册表序列化失败：{e}"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("注册表目录创建失败：{e}"))?;
        }
        std::fs::write(path, json).map_err(|e| format!("能力注册表写入失败：{e}"))
    }

    /// 从 JSON 快照加载注册表（崩溃/重启恢复；文件不存在或损坏返回 Err，不静默降级）。
    pub fn load(path: &Path) -> Result<Self, String> {
        let json = std::fs::read_to_string(path).map_err(|e| format!("能力注册表读取失败：{e}"))?;
        let snapshot: RegistrySnapshot =
            serde_json::from_str(&json).map_err(|e| format!("能力注册表解析失败：{e}"))?;
        let reg = Self::default();
        for card in snapshot.cards {
            reg.register(card);
        }
        if let Ok(mut health) = reg.health.lock() {
            *health = snapshot.health;
        }
        if let Ok(mut stats) = reg.stats.lock() {
            *stats = snapshot.stats;
        }
        Ok(reg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_match_routes_to_pick() {
        let reg = CapabilityWorkerRegistry::new();
        reg.register(
            CapabilityCard::new("w1")
                .actions(vec!["shell".to_string()])
                .models(vec!["local:qwen2.5".to_string()]),
        );
        let req = WorkerRequirement {
            actions: vec!["shell".to_string()],
            ..Default::default()
        };
        assert_eq!(reg.route(&req), RouteDecision::Pick("w1".to_string()));
    }

    #[test]
    fn hard_mismatch_rejects_explicitly() {
        let reg = CapabilityWorkerRegistry::new();
        reg.register(CapabilityCard::new("w1").models(vec!["local:a".to_string()]));
        let req = WorkerRequirement {
            models: vec!["cloud:gpt-4o".to_string()],
            min_trust: Some(TrustLevel::Local),
            ..Default::default()
        };
        match reg.route(&req) {
            RouteDecision::Reject { reasons } => {
                assert!(!reasons.is_empty());
                assert!(reasons.join(";").contains("models"));
            }
            other => panic!("应明确拒绝：{other:?}"),
        }
    }

    #[test]
    fn missing_soft_dims_degrade() {
        let reg = CapabilityWorkerRegistry::new();
        reg.register(CapabilityCard::new("w1").actions(vec!["shell".to_string()]));
        let req = WorkerRequirement {
            actions: vec!["shell".to_string(), "browser".to_string()],
            ..Default::default()
        };
        match reg.route(&req) {
            RouteDecision::Degrade { worker, missing } => {
                assert_eq!(worker, "w1");
                assert!(missing.contains(&"action:browser".to_string()));
            }
            other => panic!("应降级而非拒绝：{other:?}"),
        }
    }

    #[test]
    fn evaluate_worker_unknown_is_none() {
        let reg = CapabilityWorkerRegistry::new();
        assert!(reg
            .evaluate_worker("nope", &WorkerRequirement::default())
            .is_none());
    }
}
