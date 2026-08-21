// R11:slo 质量收尾完成。
// R12:slo 完成，待主控接线（错误预算一致 / 周期报告 / 周报 / 告警，数据面完整）
// R12:slo 复核完成（错误预算一致/周报可生成，无需改动）。
//! SLO 注册表与错误预算（R7 Agent 4 Wave 2 + R8 周期聚合 + R9 告警/周报）：`/metrics/slo` 数据面。
//! R9:slo 告警与周报完成，待主控接线（`check_alerts_global` 供定时评估并发布告警事件；
//! `write_weekly_report` 输出 JSON/CSV 到 `<data>/slo-reports/`）。
//!
//! 依据 `builGoal/综合技术开发文档-2026-08-16.md` §6.5 SLO 指标表定义基线：
//! - `ipc`：IPC 往返 <5ms（p95）
//! - `tool_schedule`：工具调度（内部调用不含网络）<10ms（p95）
//! - `panel_wake`：面板唤起 <150ms（p95）
//! - `http_success`：本地 HTTP 请求成功 ≥99.9%（会话内）
//! - `audit_zero_loss`：审计关键事件零丢失 100%（成功率）
//!
//! 提供 `check_slo`（记录一次观察）、`error_budget`（错误预算）、`report`（JSON 报告）、
//! `report_period`/`report_weekly_global`（R8 周期聚合）、`AlertRegistry`（R9 告警规则/
//! 连续违规判定/结构化告警事件）、`write_weekly_report`（R9 周报 JSON/CSV 落盘）。
//! 本模块不引用 `crate::`/`super::`，可被测试以 `#[path] mod` 独立编译。

// 主控收尾接线说明：lib 目标仅引用 report_global（经 observability_api 的
// /metrics/slo handler 注册探针）；其余符号由 slo_tests/observability_tests 以
// #[path] 独立编译使用，lib 内属"测试面符号"。同 event_stream.rs 模块级 allow 做法。
#![allow(dead_code)]

use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// 延迟类 SLO 的窗口样本上限（环形丢弃最旧）。
pub const SAMPLE_WINDOW_CAP: usize = 4096;

/// 单个 SLO 定义。
#[derive(Debug, Clone)]
pub struct Slo {
    pub name: &'static str,
    pub description: &'static str,
    /// 延迟目标（ms，p95）。None 表示成功率型 SLO。
    pub target_ms: Option<u64>,
    /// 成功率下限（0.0-1.0，如 0.999 / 1.0）。None 表示延迟型 SLO。
    pub success_floor: Option<f64>,
}

/// 一次观察样本。
#[derive(Debug, Clone)]
pub struct SloSample {
    pub latency_ms: Option<u64>,
    pub ok: bool,
    pub at_ms: u64,
}

/// 一个 SLO 的运行状态：样本窗口 + 违规计数。
/// `samples`/`violations` 用 Arc 共享：`SloRegistry::get` 返回的克隆与注册表内
/// 原始状态写同一份数据（record 后任意持有者都能读到）。
#[derive(Debug, Clone)]
pub struct SloState {
    pub slo: Slo,
    samples: Arc<Mutex<Vec<SloSample>>>,
    violations: Arc<Mutex<u64>>,
}

impl SloState {
    pub fn new(slo: Slo) -> Self {
        Self {
            slo,
            samples: Arc::new(Mutex::new(Vec::new())),
            violations: Arc::new(Mutex::new(0)),
        }
    }

    /// 记录一次观察；返回本次是否达标。
    pub fn record(&self, latency_ms: Option<u64>, ok: bool) -> bool {
        let within = self.slo_ok(latency_ms, ok);
        let mut samples = self.samples.lock().unwrap_or_else(|e| e.into_inner());
        samples.push(SloSample {
            latency_ms,
            ok,
            at_ms: now_ms(),
        });
        while samples.len() > SAMPLE_WINDOW_CAP {
            samples.remove(0);
        }
        if !within {
            let mut violations = self.violations.lock().unwrap_or_else(|e| e.into_inner());
            *violations += 1;
        }
        within
    }

    /// 窗口样本数。
    pub fn sample_count(&self) -> usize {
        self.samples.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// 累计违规次数（全历史，供报告）。
    pub fn violation_count(&self) -> u64 {
        *self.violations.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 当前窗口内延迟样本的 p95（延迟型）。
    pub fn p95_ms(&self) -> Option<u64> {
        let samples = self.samples.lock().unwrap_or_else(|e| e.into_inner());
        let mut latencies: Vec<u64> = samples.iter().filter_map(|s| s.latency_ms).collect();
        latencies.sort_unstable();
        percentile(&latencies, 0.95)
    }

    /// 当前窗口成功率（0.0-1.0）；无样本返回 None。
    pub fn success_rate(&self) -> Option<f64> {
        let samples = self.samples.lock().unwrap_or_else(|e| e.into_inner());
        if samples.is_empty() {
            return None;
        }
        let ok = samples.iter().filter(|s| s.ok).count();
        Some(ok as f64 / samples.len() as f64)
    }

    /// R8：周期窗口内的样本切片（`days` 天内，含边界）。0 或更小视为全窗口。
    pub fn samples_in_window(&self, days: u64) -> Vec<SloSample> {
        let samples = self.samples.lock().unwrap_or_else(|e| e.into_inner());
        if days == 0 {
            return samples.clone();
        }
        let cutoff = now_ms().saturating_sub(days.saturating_mul(86_400_000));
        samples
            .iter()
            .filter(|s| s.at_ms >= cutoff)
            .cloned()
            .collect()
    }

    /// R8：指定窗口的统计（p95/成功率/违规数/预算），供周期报告使用。
    pub fn window_stats(&self, days: u64) -> WindowStats {
        let samples = self.samples_in_window(days);
        let total = samples.len();
        let bad = samples
            .iter()
            .filter(|s| !self.slo_ok(s.latency_ms, s.ok))
            .count();
        let mut latencies: Vec<u64> = samples.iter().filter_map(|s| s.latency_ms).collect();
        latencies.sort_unstable();
        let success_rate = if total == 0 {
            None
        } else {
            let ok = samples.iter().filter(|s| s.ok).count();
            Some(ok as f64 / total as f64)
        };
        WindowStats {
            total,
            bad,
            p95_ms: percentile(&latencies, 0.95),
            success_rate,
        }
    }

    /// 错误预算：窗口内允许的最大违规数对比实际。
    /// 违规 = 外部 ok=false 或延迟超过目标（与 `record` 的达标判定一致）。
    pub fn error_budget(&self) -> ErrorBudget {
        let samples = self.samples.lock().unwrap_or_else(|e| e.into_inner());
        let total = samples.len();
        let bad = samples
            .iter()
            .filter(|s| !self.slo_ok(s.latency_ms, s.ok))
            .count();
        let allowed_bad = match self.slo.success_floor {
            Some(floor) => (total as f64 * (1.0 - floor)).floor() as usize,
            None => total / 5, // 延迟型按 p95：默认允许 5% 越界
        };
        let remaining = allowed_bad.saturating_sub(bad) as f64 / (allowed_bad.max(1)) as f64;
        ErrorBudget {
            total,
            bad,
            allowed_bad,
            remaining,
            within: bad <= allowed_bad,
        }
    }

    fn slo_ok(&self, latency_ms: Option<u64>, ok: bool) -> bool {
        if !ok {
            return false;
        }
        match (self.slo.target_ms, latency_ms) {
            (Some(target), Some(latency)) => latency <= target,
            (Some(_), None) => true, // 延迟型但无延迟数据（如仅标记成功）→ 视作达标
            (None, _) => true,
        }
    }
}

/// 错误预算快照。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ErrorBudget {
    pub total: usize,
    pub bad: usize,
    pub allowed_bad: usize,
    pub remaining: f64,
    pub within: bool,
}

/// R8：周期窗口统计快照。
#[derive(Debug, Clone, serde::Serialize)]
pub struct WindowStats {
    pub total: usize,
    pub bad: usize,
    pub p95_ms: Option<u64>,
    pub success_rate: Option<f64>,
}

/// SLO 注册表：名称 → 运行状态。
#[derive(Clone)]
pub struct SloRegistry {
    states: Arc<Mutex<std::collections::HashMap<&'static str, SloState>>>,
}

impl Default for SloRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SloRegistry {
    pub fn new() -> Self {
        let registry = Self {
            states: Arc::new(Mutex::new(std::collections::HashMap::new())),
        };
        for slo in default_slos() {
            registry.register(slo);
        }
        registry
    }

    pub fn register(&self, slo: Slo) {
        let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        states.insert(slo.name, SloState::new(slo));
    }

    pub fn get(&self, name: &str) -> Option<SloState> {
        self.states
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
    }

    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .states
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .map(|k| k.to_string())
            .collect();
        names.sort();
        names
    }

    pub fn is_empty(&self) -> bool {
        self.states
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }
}

/// 默认 SLO 基线（综合文档 §6.5）。
pub fn default_slos() -> Vec<Slo> {
    vec![
        Slo {
            name: "ipc",
            description: "IPC 往返（p95 <5ms）",
            target_ms: Some(5),
            success_floor: None,
        },
        Slo {
            name: "tool_schedule",
            description: "工具调度（内部调用不含网络，p95 <10ms）",
            target_ms: Some(10),
            success_floor: None,
        },
        Slo {
            name: "panel_wake",
            description: "面板唤起（p95 <150ms）",
            target_ms: Some(150),
            success_floor: None,
        },
        Slo {
            name: "http_success",
            description: "本地 HTTP 请求成功（≥99.9% 会话内）",
            target_ms: None,
            success_floor: Some(0.999),
        },
        Slo {
            name: "audit_zero_loss",
            description: "审计关键事件零丢失（100%）",
            target_ms: None,
            success_floor: Some(1.0),
        },
    ]
}

/// 全局注册表（供 observability_api / 接线方使用；测试用 `reset_global_for_test`）。
/// Mutex<Option> 使 reset 可重复生效（OnceLock 只能 set 一次，跨测试隔离不可用）。
static GLOBAL: Mutex<Option<Arc<SloRegistry>>> = Mutex::new(None);

pub fn global() -> Arc<SloRegistry> {
    let mut slot = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
    slot.get_or_insert_with(|| Arc::new(SloRegistry::new()))
        .clone()
}

/// 仅供测试：重置全局注册表。
#[allow(dead_code)] // 仅供 slo_tests / observability_tests 以 #[path] 独立编译使用。
pub fn reset_global_for_test() {
    *GLOBAL.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// 便捷记录：`check_slo(registry, name, latency_ms, ok)` → 是否达标。
/// 未注册的 SLO 名称视作宽松通过（不 panic、不注册），防止误写名造成假阴性。
pub fn check_slo(registry: &SloRegistry, name: &str, latency_ms: Option<u64>, ok: bool) -> bool {
    match registry.get(name) {
        Some(state) => state.record(latency_ms, ok),
        None => true,
    }
}

/// 全局便捷记录：`check_slo_global(name, latency_ms, ok)`。
pub fn check_slo_global(name: &str, latency_ms: Option<u64>, ok: bool) -> bool {
    check_slo(&global(), name, latency_ms, ok)
}

/// 错误预算查询。
pub fn error_budget(registry: &SloRegistry, name: &str) -> Option<ErrorBudget> {
    registry.get(name).map(|state| state.error_budget())
}

/// JSON 报告：每个 SLO 的样本数/p95/成功率/违规数/错误预算/达标状态。
pub fn report(registry: &SloRegistry) -> Value {
    let mut items: Vec<Value> = Vec::new();
    for name in registry.names() {
        if let Some(state) = registry.get(&name) {
            let budget = state.error_budget();
            items.push(json!({
                "name": state.slo.name,
                "description": state.slo.description,
                "target_ms": state.slo.target_ms,
                "success_floor": state.slo.success_floor,
                "samples": state.sample_count(),
                "p95_ms": state.p95_ms(),
                "success_rate": state.success_rate(),
                "violations": state.violation_count(),
                "error_budget": {
                    "total": budget.total,
                    "bad": budget.bad,
                    "allowed_bad": budget.allowed_bad,
                    "remaining": (budget.remaining * 1000.0).round() / 1000.0,
                    "within": budget.within,
                },
                "achieving": budget.within,
            }));
        }
    }
    json!({ "count": items.len(), "slo": items })
}

/// 全局 JSON 报告（供 /metrics/slo handler）。
pub fn report_global() -> Value {
    report(&global())
}

/// R8：周期聚合报告（`days` 天窗口，0 = 全窗口），结构同 `report` 并带 period 标记。
pub fn report_period(registry: &SloRegistry, days: u64) -> Value {
    let mut items: Vec<Value> = Vec::new();
    for name in registry.names() {
        if let Some(state) = registry.get(&name) {
            let stats = state.window_stats(days);
            let within = {
                let allowed = match state.slo.success_floor {
                    Some(floor) => (stats.total as f64 * (1.0 - floor)).floor() as usize,
                    None => stats.total / 5,
                };
                stats.bad <= allowed
            };
            items.push(json!({
                "name": state.slo.name,
                "description": state.slo.description,
                "target_ms": state.slo.target_ms,
                "success_floor": state.slo.success_floor,
                "period_days": days,
                "samples": stats.total,
                "p95_ms": stats.p95_ms,
                "success_rate": stats.success_rate,
                "violations_in_window": stats.bad,
                "achieving": within,
            }));
        }
    }
    json!({ "count": items.len(), "period_days": days, "slo": items })
}

/// R8：全局周期报告（供 soak/诊断/面板消费）。
pub fn report_period_global(days: u64) -> Value {
    report_period(&global(), days)
}

/// R8：周报聚合（7 天窗口）。
pub fn report_weekly_global() -> Value {
    report_period_global(7)
}

/// 全局达标标志（所有已观察 SLO 窗口内达标）。
pub fn global_achieving() -> bool {
    let registry = global();
    let mut all_within = true;
    for name in registry.names() {
        if let Some(state) = registry.get(&name) {
            if state.sample_count() > 0 && !state.error_budget().within {
                all_within = false;
            }
        }
    }
    all_within
}

/// 有序样本百分位（0.0-1.0）。空返回 None。
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

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ==================== R9：告警规则与告警事件 ====================

/// 告警判定种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AlertKind {
    /// 延迟型：窗口 p95 超过阈值（ms）。
    P95AboveMs,
    /// 错误预算剩余比例低于阈值（0.0-1.0）。
    BudgetRemainingBelow,
    /// 窗口违规数超过阈值。
    ViolationsAbove,
}

/// 告警规则：绑定一个 SLO + 判定种类 + 阈值 + 连续违规次数。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AlertRule {
    pub name: &'static str,
    pub slo_name: &'static str,
    pub kind: AlertKind,
    pub threshold: f64,
    /// 连续违规达到该次数才触发（防抖动）。
    pub consecutive: u32,
    /// "warning" | "critical"。
    pub severity: &'static str,
    pub description: &'static str,
}

/// 告警事件（结构化；`kind` = "firing" | "recovered"）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AlertEvent {
    pub at_ms: u64,
    pub at: String,
    pub kind: String,
    pub rule: String,
    pub slo: String,
    pub severity: String,
    pub detail: String,
    pub consecutive: u32,
    pub trace_id: Option<String>,
}

/// 规则运行状态：当前连续违规数与是否已触发（防重复告警）。
#[derive(Debug, Clone, Default)]
struct RuleState {
    consecutive: u32,
    active: bool,
}

/// 告警事件保留上限（环形丢弃最旧）。
pub const ALERT_EVENTS_CAP: usize = 256;

/// 告警监听器：接线方把告警事件发布到 event_stream / 日志。
pub type AlertListener = Box<dyn Fn(&AlertEvent) + Send + Sync>;

static ALERT_LISTENER: Mutex<Option<AlertListener>> = Mutex::new(None);

/// 注册告警监听器（主控接线：转发到 `event_stream::hub().publish_alert`）。
pub fn set_alert_listener(listener: AlertListener) {
    let mut slot = ALERT_LISTENER.lock().unwrap_or_else(|e| e.into_inner());
    *slot = Some(listener);
}

/// 仅供测试：清空告警监听器。
#[allow(dead_code)] // 仅供 slo_tests 以 #[path] 独立编译使用；lib 目标内无引用。
pub fn reset_alert_listener_for_test() {
    let mut slot = ALERT_LISTENER.lock().unwrap_or_else(|e| e.into_inner());
    *slot = None;
}

/// 告警注册表（R9）：规则 + 运行状态 + 最近事件（有界）。
#[derive(Clone)]
pub struct AlertRegistry {
    rules: Arc<Mutex<Vec<AlertRule>>>,
    states: Arc<Mutex<HashMap<String, RuleState>>>,
    events: Arc<Mutex<VecDeque<AlertEvent>>>,
}

impl Default for AlertRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AlertRegistry {
    pub fn new() -> Self {
        let registry = Self {
            rules: Arc::new(Mutex::new(Vec::new())),
            states: Arc::new(Mutex::new(HashMap::new())),
            events: Arc::new(Mutex::new(VecDeque::new())),
        };
        for rule in default_alert_rules() {
            registry.register_rule(rule);
        }
        registry
    }

    pub fn register_rule(&self, rule: AlertRule) {
        let mut rules = self.rules.lock().unwrap_or_else(|e| e.into_inner());
        if !rules.iter().any(|r| r.name == rule.name) {
            rules.push(rule);
        }
    }

    pub fn rules(&self) -> Vec<AlertRule> {
        self.rules.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn events(&self) -> Vec<AlertEvent> {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// 评估全部规则：返回本次新触发/恢复的事件（已激活的规则不重复触发）。
    pub fn check(&self, registry: &SloRegistry, trace_id: Option<&str>) -> Vec<AlertEvent> {
        let rules = self.rules();
        let mut fired = Vec::new();
        for rule in rules {
            let Some(state) = registry.get(rule.slo_name) else {
                continue;
            };
            let violating = self.evaluate(&rule, &state);
            let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
            let rs = states.entry(rule.name.to_string()).or_default();
            if violating {
                rs.consecutive += 1;
                if rs.consecutive >= rule.consecutive && !rs.active {
                    rs.active = true;
                    let event = AlertEvent {
                        at_ms: now_ms(),
                        at: chrono::Utc::now().to_rfc3339(),
                        kind: "firing".to_string(),
                        rule: rule.name.to_string(),
                        slo: rule.slo_name.to_string(),
                        severity: rule.severity.to_string(),
                        detail: self.describe(&rule, &state),
                        consecutive: rs.consecutive,
                        trace_id: trace_id.map(str::to_string),
                    };
                    self.push_event(event.clone());
                    fired.push(event);
                }
            } else if rs.active {
                rs.active = false;
                rs.consecutive = 0;
                let event = AlertEvent {
                    at_ms: now_ms(),
                    at: chrono::Utc::now().to_rfc3339(),
                    kind: "recovered".to_string(),
                    rule: rule.name.to_string(),
                    slo: rule.slo_name.to_string(),
                    severity: rule.severity.to_string(),
                    detail: format!("{} 恢复达标", rule.slo_name),
                    consecutive: 0,
                    trace_id: trace_id.map(str::to_string),
                };
                self.push_event(event.clone());
                fired.push(event);
            } else {
                rs.consecutive = 0;
            }
        }
        fired
    }

    /// 单条规则违规判定。
    fn evaluate(&self, rule: &AlertRule, state: &SloState) -> bool {
        match rule.kind {
            AlertKind::P95AboveMs => state
                .p95_ms()
                .map(|p| p as f64 > rule.threshold)
                .unwrap_or(false),
            AlertKind::BudgetRemainingBelow => {
                let remaining = state.error_budget().remaining;
                remaining < rule.threshold
            }
            AlertKind::ViolationsAbove => state.window_stats(0).bad as f64 > rule.threshold,
        }
    }

    fn describe(&self, rule: &AlertRule, state: &SloState) -> String {
        match rule.kind {
            AlertKind::P95AboveMs => format!(
                "{} 窗口 p95={}ms 超过阈值 {}ms",
                rule.slo_name,
                state
                    .p95_ms()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                rule.threshold
            ),
            AlertKind::BudgetRemainingBelow => {
                let budget = state.error_budget();
                format!(
                    "{} 错误预算剩余 {:.1}% 低于阈值 {:.0}%",
                    rule.slo_name,
                    budget.remaining * 100.0,
                    rule.threshold * 100.0
                )
            }
            AlertKind::ViolationsAbove => format!(
                "{} 违规 {} 次超过阈值 {}",
                rule.slo_name,
                state.window_stats(0).bad,
                rule.threshold
            ),
        }
    }

    fn push_event(&self, event: AlertEvent) {
        {
            let mut events = self.events.lock().unwrap_or_else(|e| e.into_inner());
            events.push_back(event.clone());
            while events.len() > ALERT_EVENTS_CAP {
                events.pop_front();
            }
        }
        let guard = ALERT_LISTENER.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(listener) = guard.as_ref() {
            listener(&event);
        }
    }
}

/// 默认告警规则基线（R9）。
pub fn default_alert_rules() -> Vec<AlertRule> {
    vec![
        AlertRule {
            name: "ipc-p95-slow",
            slo_name: "ipc",
            kind: AlertKind::P95AboveMs,
            threshold: 5.0,
            consecutive: 3,
            severity: "warning",
            description: "IPC 往返 p95 连续超过 5ms",
        },
        AlertRule {
            name: "tool-schedule-p95-slow",
            slo_name: "tool_schedule",
            kind: AlertKind::P95AboveMs,
            threshold: 10.0,
            consecutive: 5,
            severity: "warning",
            description: "工具调度 p95 连续超过 10ms",
        },
        AlertRule {
            name: "panel-wake-p95-slow",
            slo_name: "panel_wake",
            kind: AlertKind::P95AboveMs,
            threshold: 150.0,
            consecutive: 3,
            severity: "warning",
            description: "面板唤起 p95 连续超过 150ms",
        },
        AlertRule {
            name: "http-error-budget-low",
            slo_name: "http_success",
            kind: AlertKind::BudgetRemainingBelow,
            threshold: 0.3,
            consecutive: 2,
            severity: "critical",
            description: "HTTP 成功率错误预算剩余低于 30%",
        },
        AlertRule {
            name: "audit-zero-loss-broken",
            slo_name: "audit_zero_loss",
            kind: AlertKind::ViolationsAbove,
            threshold: 0.0,
            consecutive: 1,
            severity: "critical",
            description: "审计关键事件出现丢失",
        },
    ]
}

/// 全局告警注册表。
static ALERT_REGISTRY: Mutex<Option<Arc<AlertRegistry>>> = Mutex::new(None);

pub fn alert_registry() -> Arc<AlertRegistry> {
    let mut slot = ALERT_REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    slot.get_or_insert_with(|| Arc::new(AlertRegistry::new()))
        .clone()
}

/// 仅供测试：重置全局告警注册表。
#[allow(dead_code)] // 仅供 slo_tests 以 #[path] 独立编译使用；lib 目标内无引用。
pub fn reset_alert_registry_for_test() {
    *ALERT_REGISTRY.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// 全局评估便捷：评估全部告警规则并返回新事件（接线方可定时调用）。
pub fn check_alerts_global(trace_id: Option<&str>) -> Vec<AlertEvent> {
    alert_registry().check(&global(), trace_id)
}

/// 告警 JSON（供 /metrics/slo/alerts 探针 / 诊断包 / 面板消费）：
/// `{count, rules:[...], alerts:[最近 limit 条（新→旧）]}`。
pub fn alerts_json(limit: usize) -> Value {
    let registry = alert_registry();
    let events = registry.events();
    let items: Vec<Value> = events
        .into_iter()
        .rev()
        .take(limit)
        .map(|e| json!(e))
        .collect();
    json!({
        "count": items.len(),
        "rules": registry.rules(),
        "alerts": items,
    })
}

// ==================== R9：周报输出 ====================

/// 周报输出目录名（相对 data 根）。
pub const SLO_REPORTS_DIR: &str = "slo-reports";

/// 生成周报（JSON + CSV）到 `<data>/slo-reports/`；返回 JSON 主文件路径。
/// 内容 = `report_weekly_global()`（7 天窗口周期聚合）。
pub fn write_weekly_report(data_dir: &Path) -> std::io::Result<PathBuf> {
    let dir = data_dir.join(SLO_REPORTS_DIR);
    std::fs::create_dir_all(&dir)?;
    let date = chrono::Utc::now().format("%Y-%m-%d");
    let json_path = dir.join(format!("slo-weekly-{date}.json"));
    let csv_path = dir.join(format!("slo-weekly-{date}.csv"));
    let weekly = report_weekly_global();
    let json_text = serde_json::to_string_pretty(&weekly)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&json_path, json_text)?;
    let mut csv = String::from("slo,p95_ms,success_rate,samples,violations,achieving\n");
    if let Some(items) = weekly["slo"].as_array() {
        for item in items {
            csv.push_str(&format!(
                "{},{},{},{},{},{}\n",
                item["name"].as_str().unwrap_or(""),
                item["p95_ms"]
                    .as_i64()
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                item["success_rate"]
                    .as_f64()
                    .map(|v| format!("{v:.4}"))
                    .unwrap_or_default(),
                item["samples"].as_u64().unwrap_or(0),
                item["violations_in_window"].as_u64().unwrap_or(0),
                item["achieving"].as_bool().unwrap_or(false),
            ));
        }
    }
    std::fs::write(&csv_path, csv)?;
    Ok(json_path)
}
