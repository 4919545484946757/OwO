//! Goal：目标→计划→并行 worker→验证→仲裁→恢复 的编排层（§12 底座 / 续写 §15）。
//!
//! - [`Goal`]：目标对象（objective / 状态机 / 预算 / 验收条件）。
//! - [`Worker`] + [`WorkerRegistry`]：步骤执行抽象（测试注入 MockWorker；
//!   真实接入 `Agent::run_subagent` 由主控后续做，本模块只读引用 agent 语义）。
//! - [`GoalRunner`]：拓扑 wave 调度 + 并行度上限 + 步内重试 + 验证断言 +
//!   replan（只重建未完成子图）+ 预算熔断 + abort + 持久化恢复（已完成步骤不重跑）+ 全程审计。

use crate::audit::AuditLog;
use crate::plan::{verify_output, Plan, StepSpec, StepStatus, VerificationSpec};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 目标状态机：Pending→Planning→Running→Verifying→Succeeded/Failed/Aborted。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoalStatus {
    Pending,
    Planning,
    Running,
    Verifying,
    Succeeded,
    Failed,
    Aborted,
}

impl GoalStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            GoalStatus::Succeeded | GoalStatus::Failed | GoalStatus::Aborted
        )
    }
}

/// 目标预算（熔断阈值）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct GoalBudget {
    /// 最大执行步骤数（含重试与 replan 消耗）。
    pub max_steps: u32,
    /// 每步最大重试次数（超出直接失败）。
    pub max_retries_per_step: u32,
    /// 全局重试次数上限（预算熔断）。
    pub max_total_retries: u32,
    /// 最大 replan 次数。
    pub max_replans: u32,
    /// 最大执行时长（秒，0 = 不限）。
    pub max_duration_secs: u64,
}

impl Default for GoalBudget {
    fn default() -> Self {
        Self {
            max_steps: 200,
            max_retries_per_step: 2,
            max_total_retries: 10,
            max_replans: 2,
            max_duration_secs: 0,
        }
    }
}

/// 目标对象。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goal {
    pub id: String,
    /// 目标描述（objective）。
    pub objective: String,
    pub status: GoalStatus,
    pub budget: GoalBudget,
    /// 目标级验收条件（全部通过才 Succeeded）。
    #[serde(default)]
    pub acceptance: Vec<VerificationSpec>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Goal {
    pub fn new(id: impl Into<String>, objective: impl Into<String>) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: id.into(),
            objective: objective.into(),
            status: GoalStatus::Pending,
            budget: GoalBudget::default(),
            acceptance: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
            error: None,
        }
    }

    pub fn transition(&mut self, status: GoalStatus) {
        self.status = status;
        self.updated_at = chrono::Utc::now().to_rfc3339();
    }
}

/// Worker：步骤执行抽象。真实接入 `Agent::run_subagent`（主控后续做）。
#[async_trait]
pub trait Worker: Send + Sync {
    fn name(&self) -> &str;
    async fn run(&self, input: &serde_json::Value) -> Result<String, String>;
}

/// 按名派发的 worker 注册表。
#[derive(Clone, Default)]
pub struct WorkerRegistry {
    workers: Arc<Mutex<HashMap<String, Arc<dyn Worker>>>>,
}

impl WorkerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, worker: Arc<dyn Worker>) {
        let name = worker.name().to_string();
        if let Ok(mut workers) = self.workers.lock() {
            workers.insert(name, worker);
        }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Worker>> {
        self.workers.lock().ok().and_then(|w| w.get(name).cloned())
    }
}

/// 单步执行记录（运行状态，持久化恢复的依据）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRecord {
    pub step_id: String,
    pub status: StepStatus,
    pub attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 一次运行的完整状态（可整体持久化：<dir>/<run_id>.json）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalRunState {
    pub run_id: String,
    pub goal: Goal,
    pub plan: Plan,
    /// step_id → 执行记录。
    pub records: BTreeMap<String, StepRecord>,
    /// 全局已执行动作数（预算）。
    pub steps_taken: u32,
    /// 全局重试次数。
    pub total_retries: u32,
    /// 已执行 replan 次数。
    pub replan_count: u32,
    pub started_at: String,
    /// 顶层审计事件（文本；可选注入 AuditLog 同步写）。
    pub events: Vec<String>,
    /// 调度器是否已 abort。
    pub aborted: bool,
}

impl GoalRunState {
    pub fn new(goal: Goal, plan: Plan) -> Self {
        let records = plan
            .steps
            .iter()
            .map(|s| {
                (
                    s.id.clone(),
                    StepRecord {
                        step_id: s.id.clone(),
                        status: StepStatus::Pending,
                        attempts: 0,
                        output: None,
                        error: None,
                    },
                )
            })
            .collect();
        Self {
            run_id: format!("run-{}", chrono::Utc::now().timestamp_millis()),
            goal,
            plan,
            records,
            steps_taken: 0,
            total_retries: 0,
            replan_count: 0,
            started_at: chrono::Utc::now().to_rfc3339(),
            events: Vec::new(),
            aborted: false,
        }
    }

    /// 持久化：`<dir>/<run_id>.json`（含目标/计划/步骤记录，重启恢复）。
    pub fn persist(&self, dir: &Path) -> Result<PathBuf, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建运行目录失败：{e}"))?;
        let path = dir.join(format!("{}.json", self.run_id));
        let json =
            serde_json::to_string_pretty(self).map_err(|e| format!("运行状态序列化失败：{e}"))?;
        std::fs::write(&path, json).map_err(|e| format!("运行状态写入失败：{e}"))?;
        Ok(path)
    }

    /// 从磁盘恢复运行状态。
    pub fn load(dir: &Path, run_id: &str) -> Result<GoalRunState, String> {
        let path = dir.join(format!("{run_id}.json"));
        let json = std::fs::read_to_string(&path)
            .map_err(|e| format!("运行状态 {run_id} 读取失败：{e}（{path:?}）"))?;
        serde_json::from_str(&json).map_err(|e| format!("运行状态 {run_id} 解析失败：{e}"))
    }
}

/// 调度器配置。
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// 并行度上限（wave 内并发执行的步骤数）。
    pub max_parallel: usize,
    /// 持久化目录（Some 时每步执行后落盘；恢复时读取）。
    pub persist_dir: Option<PathBuf>,
    /// 允许 replan（验证/执行失败时重建未完成子图）。
    pub allow_replan: bool,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            max_parallel: 4,
            persist_dir: None,
            allow_replan: true,
        }
    }
}

/// Goal/Plan 调度器：wave 拓扑 + 并行限流 + 重试 + 验证 + replan + 恢复 + 审计。
pub struct GoalRunner {
    pub state: GoalRunState,
    config: RunnerConfig,
    /// 可选审计（与顶层 events 同步写）。
    audit: Option<Arc<Mutex<AuditLog>>>,
    /// 跨并发任务的 abort 标志（随 state.aborted 初始化）。
    aborted_flag: Arc<std::sync::atomic::AtomicBool>,
}

impl GoalRunner {
    pub fn new(goal: Goal, plan: Plan, config: RunnerConfig) -> Self {
        Self {
            state: GoalRunState::new(goal, plan),
            config,
            audit: None,
            aborted_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// 从持久化状态恢复（崩溃重启；已完成步骤不重跑）。
    pub fn from_state(state: GoalRunState, config: RunnerConfig) -> Self {
        let aborted = state.aborted;
        Self {
            state,
            config,
            audit: None,
            aborted_flag: Arc::new(std::sync::atomic::AtomicBool::new(aborted)),
        }
    }

    /// 注入审计日志（每次动作写一条 audit 记录 + 顶层事件）。
    pub fn attach_audit(&mut self, log: Arc<Mutex<AuditLog>>) {
        self.audit = Some(log);
    }

    fn log(&mut self, event: &str, detail: impl Into<String>) {
        let detail = detail.into();
        self.state.events.push(format!("{event}: {detail}"));
        if let Some(log) = &self.audit {
            if let Ok(mut log) = log.lock() {
                log.record(
                    &self.state.goal.id,
                    event,
                    Some(format!("goal/{}", self.state.plan.goal_id)),
                    None,
                    detail,
                );
            }
        }
    }

    fn record_mut(&mut self, step_id: &str) -> &mut StepRecord {
        self.state
            .records
            .get_mut(step_id)
            .expect("步骤记录必须存在")
    }

    /// 取消执行：abort 标志置位，未完成步骤标记 Aborted 保留现场。
    pub fn abort(&mut self) {
        self.state.aborted = true;
        self.aborted_flag
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.mark_remaining(StepStatus::Aborted);
        self.log("goal.abort", "调度器收到 abort 请求");
        self.state.goal.transition(GoalStatus::Aborted);
        self.persist_if_needed();
    }

    /// 执行计划（恢复时已完成步骤自动跳过）。返回目标终态。
    pub async fn run(&mut self, workers: &WorkerRegistry) -> Result<GoalStatus, String> {
        if self.state.goal.status.is_terminal() {
            return Ok(self.state.goal.status);
        }
        if self.state.aborted {
            return Ok(GoalStatus::Aborted);
        }
        self.state.goal.transition(GoalStatus::Running);
        self.log("goal.start", format!("目标 {}", self.state.goal.objective));

        let started = std::time::Instant::now();
        let budget = self.state.goal.budget;
        let max_parallel = self.config.max_parallel.max(1);
        let abort_flag = Arc::clone(&self.aborted_flag);
        let workers = workers.clone();

        loop {
            if self.state.aborted {
                self.mark_remaining(StepStatus::Aborted);
                self.state.goal.transition(GoalStatus::Aborted);
                self.persist_if_needed();
                return Ok(GoalStatus::Aborted);
            }
            // 时长预算熔断。
            if budget.max_duration_secs > 0
                && started.elapsed().as_secs() >= budget.max_duration_secs
            {
                return self.fail_goal(format!(
                    "预算熔断：执行时长超过 {}s",
                    budget.max_duration_secs
                ));
            }
            // 步骤数预算熔断。
            if self.state.steps_taken >= budget.max_steps {
                return self.fail_goal(format!(
                    "预算熔断：步骤数超过 {}（steps_taken={}）",
                    budget.max_steps, self.state.steps_taken
                ));
            }
            // 全局重试预算熔断。
            if self.state.total_retries >= budget.max_total_retries {
                return self.fail_goal(format!(
                    "预算熔断：全局重试次数超过 {}（total_retries={}）",
                    budget.max_total_retries, self.state.total_retries
                ));
            }

            // 计算当前就绪步骤：依赖全部 Succeeded 且自身未完成。
            let ready: Vec<StepSpec> = self
                .state
                .plan
                .steps
                .iter()
                .filter(|step| {
                    let record = &self.state.records[&step.id];
                    record.status.can_resume() && self.deps_succeeded(step)
                })
                .cloned()
                .collect();
            if ready.is_empty() {
                // 检查是否全部成功 → 目标验收。
                if self
                    .state
                    .plan
                    .steps
                    .iter()
                    .all(|s| self.state.records[&s.id].status == StepStatus::Succeeded)
                {
                    return self.verify_goal();
                }
                if self.state.replan_count >= budget.max_replans {
                    return self.fail_goal("replan 次数超限，未完成步骤无法恢复".to_string());
                }
                return self.fail_goal("死锁：存在未完成步骤但无就绪步骤".to_string());
            }

            // 并行执行就绪步骤（JoinSet + max_parallel 限流）。
            let mut set = tokio::task::JoinSet::new();
            let mut pending: std::collections::VecDeque<StepSpec> = ready.into_iter().collect();
            let mut discard: Vec<StepSpec> = Vec::new();
            while let Some(step) = pending.pop_front() {
                while set.len() >= max_parallel {
                    match self.merge_step_outcome(set.join_next().await, &mut discard) {
                        Ok(()) => {}
                        Err(reason) => return self.fail_goal(format!("预算熔断：{reason}")),
                    }
                }
                self.log(
                    "goal.step.start",
                    format!("步骤 {}（worker {}）", step.id, step.worker),
                );
                set.spawn(run_step_attempts(
                    workers.clone(),
                    step,
                    budget,
                    Arc::clone(&abort_flag),
                ));
            }
            let mut failed: Vec<StepSpec> = Vec::new();
            while let Some(joined) = set.join_next().await {
                match self.merge_step_outcome(Some(joined), &mut failed) {
                    Ok(()) => {}
                    Err(reason) => return self.fail_goal(format!("预算熔断：{reason}")),
                }
            }

            if self.state.aborted {
                self.mark_remaining(StepStatus::Aborted);
                self.state.goal.transition(GoalStatus::Aborted);
                self.persist_if_needed();
                return Ok(GoalStatus::Aborted);
            }

            if !failed.is_empty() {
                if !self.config.allow_replan {
                    return self.fail_goal(format!(
                        "步骤失败且 replan 未启用：{:?}",
                        failed.iter().map(|s| s.id.as_str()).collect::<Vec<_>>()
                    ));
                }
                if self.state.replan_count >= budget.max_replans {
                    return self.fail_goal(format!(
                        "步骤失败且 replan 次数超限（{}）：{:?}",
                        budget.max_replans,
                        failed.iter().map(|s| s.id.as_str()).collect::<Vec<_>>()
                    ));
                }
                self.replan(&failed);
            }
        }
    }

    /// 合并一个步骤的并发执行结果到运行状态。失败步骤加入 failed；预算熔断返回 Err(reason)。
    fn merge_step_outcome(
        &mut self,
        joined: Option<Result<StepOutcome, tokio::task::JoinError>>,
        failed: &mut Vec<StepSpec>,
    ) -> Result<(), String> {
        let outcome = match joined {
            Some(Ok(outcome)) => outcome,
            Some(Err(e)) => return Err(format!("步骤任务 panic：{e}")),
            None => return Err("步骤任务丢失".to_string()),
        };
        self.state.steps_taken = self.state.steps_taken.saturating_add(outcome.attempts);
        self.state.total_retries = self.state.total_retries.saturating_add(outcome.attempts);
        match outcome.result {
            StepResult::Ok { output } => {
                let record = self.record_mut(&outcome.step_id);
                record.status = StepStatus::Succeeded;
                record.attempts = outcome.attempts;
                record.output = Some(output.clone());
                record.error = None;
                self.log(
                    "goal.step.succeeded",
                    format!(
                        "步骤 {} 通过（{} 次尝试）",
                        outcome.step_id, outcome.attempts
                    ),
                );
                self.persist_if_needed();
            }
            StepResult::Retried { error } => {
                let record = self.record_mut(&outcome.step_id);
                record.status = StepStatus::Failed;
                record.attempts = outcome.attempts;
                record.error = Some(error.clone());
                self.log(
                    "goal.step.failed",
                    format!("步骤 {} 失败：{error}", outcome.step_id),
                );
                if let Some(step) = self.state.plan.step(&outcome.step_id) {
                    failed.push(step.clone());
                }
            }
            StepResult::Budget { reason } => {
                return Err(reason);
            }
        }
        Ok(())
    }

    /// replan：重置失败步骤及其未完成的后代子图（已 Succeeded 步骤保留），重新调度。
    fn replan(&mut self, failed: &[StepSpec]) {
        self.state.replan_count += 1;
        self.log(
            "goal.replan",
            format!(
                "第 {} 次 replan：重置 {}",
                self.state.replan_count,
                failed
                    .iter()
                    .map(|s| s.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
        // 收集受影响的未完成后代（依赖链中包含 failed 步骤的）。
        let mut to_reset: Vec<String> = Vec::new();
        for step in &self.state.plan.steps {
            if self.state.records[&step.id].status == StepStatus::Succeeded {
                continue;
            }
            if self.depends_on_any(
                step,
                &failed.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ) {
                to_reset.push(step.id.clone());
            }
        }
        for step_id in &to_reset {
            let record = self.record_mut(step_id);
            record.status = StepStatus::Pending;
            record.error = None;
            record.attempts = 0;
        }
        // 失败的步骤本身也要重置（含在 to_reset 中，因为 depends_on_any 对自身成立时）——
        // 显式重置避免依赖判断遗漏。
        for step in failed {
            if let Some(record) = self.state.records.get_mut(&step.id) {
                if !to_reset.contains(&step.id) {
                    record.status = StepStatus::Pending;
                    record.error = None;
                    record.attempts = 0;
                }
            }
        }
        self.persist_if_needed();
    }

    fn depends_on_any(&self, step: &StepSpec, targets: &[&str]) -> bool {
        let mut stack: Vec<&str> = step.depends_on.iter().map(|s| s.as_str()).collect();
        let mut visited = std::collections::HashSet::new();
        while let Some(dep) = stack.pop() {
            if targets.contains(&dep) {
                return true;
            }
            if !visited.insert(dep) {
                continue;
            }
            if let Some(dep_step) = self.state.plan.step(dep) {
                stack.extend(dep_step.depends_on.iter().map(|s| s.as_str()));
            }
        }
        false
    }

    fn deps_succeeded(&self, step: &StepSpec) -> bool {
        step.depends_on.iter().all(|dep| {
            self.state
                .records
                .get(dep)
                .map(|r| r.status == StepStatus::Succeeded)
                .unwrap_or(false)
        })
    }

    /// 目标级验收：全部 acceptance 通过才 Succeeded。
    fn verify_goal(&mut self) -> Result<GoalStatus, String> {
        self.state.goal.transition(GoalStatus::Verifying);
        self.log("goal.verifying", "全部步骤成功，进入目标验收");
        for spec in &self.state.goal.acceptance {
            // 验收条件对"最终汇总输出"断言：取全部成功步骤输出拼接。
            let summary = self
                .state
                .records
                .values()
                .filter(|r| r.status == StepStatus::Succeeded)
                .filter_map(|r| r.output.clone())
                .collect::<Vec<_>>()
                .join("\n");
            if let Err(e) = verify_output(spec, &summary) {
                return self.fail_goal(format!("目标验收失败：{e}"));
            }
        }
        self.state.goal.transition(GoalStatus::Succeeded);
        self.log("goal.succeeded", "目标验收通过");
        self.persist_if_needed();
        Ok(GoalStatus::Succeeded)
    }

    fn fail_goal(&mut self, reason: String) -> Result<GoalStatus, String> {
        self.state.goal.error = Some(reason.clone());
        self.state.goal.transition(GoalStatus::Failed);
        self.log("goal.failed", reason);
        self.persist_if_needed();
        Ok(GoalStatus::Failed)
    }

    fn mark_remaining(&mut self, status: StepStatus) {
        for record in self.state.records.values_mut() {
            if !record.status.is_terminal() {
                record.status = status;
            }
        }
    }

    fn persist_if_needed(&mut self) {
        if let Some(dir) = &self.config.persist_dir {
            let _ = self.state.persist(dir);
        }
    }
}

// ---------- 并发执行辅助 ----------

/// 步骤执行结果。
enum StepResult {
    Ok { output: String },
    Retried { error: String },
    Budget { reason: String },
}

/// 单个步骤的并发执行产出（含尝试次数，供预算合并）。
struct StepOutcome {
    step_id: String,
    attempts: u32,
    result: StepResult,
}

/// 独立执行一个步骤的尝试循环（worker 调用 + 验证断言；不触碰 runner 状态）。
/// 预算在任务内按 `budget.max_steps` 粗略封顶，全局熔断由 run() 合并后校验。
async fn run_step_attempts(
    workers: WorkerRegistry,
    step: StepSpec,
    budget: GoalBudget,
    aborted: Arc<std::sync::atomic::AtomicBool>,
) -> StepOutcome {
    let worker = match workers.get(&step.worker) {
        Some(worker) => worker,
        None => {
            return StepOutcome {
                step_id: step.id.clone(),
                attempts: 0,
                result: StepResult::Retried {
                    error: format!("worker 未注册：{}", step.worker),
                },
            }
        }
    };
    let max_attempts = step.retries + 1;
    let mut attempts = 0u32;
    while attempts < max_attempts {
        if aborted.load(std::sync::atomic::Ordering::SeqCst) {
            return StepOutcome {
                step_id: step.id.clone(),
                attempts,
                result: StepResult::Retried {
                    error: "调度器已 abort".to_string(),
                },
            };
        }
        if attempts >= budget.max_steps {
            return StepOutcome {
                step_id: step.id.clone(),
                attempts,
                result: StepResult::Budget {
                    reason: format!("步骤数超过 {}", budget.max_steps),
                },
            };
        }
        attempts += 1;
        match worker.run(&step.input).await {
            Ok(output) => {
                if let Some(spec) = &step.verify {
                    if let Err(e) = verify_output(spec, &output) {
                        if attempts >= max_attempts {
                            return StepOutcome {
                                step_id: step.id.clone(),
                                attempts,
                                result: StepResult::Retried { error: e },
                            };
                        }
                        continue;
                    }
                }
                return StepOutcome {
                    step_id: step.id.clone(),
                    attempts,
                    result: StepResult::Ok { output },
                };
            }
            Err(e) => {
                if attempts >= max_attempts {
                    return StepOutcome {
                        step_id: step.id.clone(),
                        attempts,
                        result: StepResult::Retried { error: e },
                    };
                }
            }
        }
    }
    StepOutcome {
        step_id: step.id.clone(),
        attempts,
        result: StepResult::Retried {
            error: "未知错误".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goal_status_machine_transitions() {
        let mut goal = Goal::new("g1", "测试目标");
        assert_eq!(goal.status, GoalStatus::Pending);
        goal.transition(GoalStatus::Planning);
        goal.transition(GoalStatus::Running);
        goal.transition(GoalStatus::Verifying);
        goal.transition(GoalStatus::Succeeded);
        assert!(goal.status.is_terminal());
        assert!(!GoalStatus::Running.is_terminal());
    }

    #[test]
    fn run_state_serde_roundtrip() {
        let plan = Plan::new("p1", "g1");
        let state = GoalRunState::new(Goal::new("g1", "目标"), plan);
        let json = serde_json::to_string(&state).unwrap();
        let restored: GoalRunState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.run_id, state.run_id);
        assert_eq!(restored.records.len(), 0);
    }
}
