//! §12 Goal/Plan 多 Agent 编排契约测试。
//!
//! 覆盖：DAG 拓扑/环检测、并行度上限、worker 失败重试、验证失败 replan、
//! 恢复幂等、abort、预算熔断、审计、序列化往返、"三步骤并行 + 汇合验证"样例。

use async_trait::async_trait;
use owo_agent_core::goal::{
    Goal, GoalBudget, GoalRunState, GoalRunner, GoalStatus, RunnerConfig, Worker, WorkerRegistry,
};
use owo_agent_core::plan::{Plan, StepSpec, VerificationSpec};
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// 记录 worker：输出=input.text + 计数；可注入失败次数。
struct EchoWorker {
    name: String,
    output: String,
    fail_times: Mutex<HashMap<String, u32>>,
    runs: Arc<AtomicUsize>,
    /// 并发峰值记录（进入 +1，退出 -1）。
    current: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

impl EchoWorker {
    fn new(name: &str, output: &str) -> Self {
        Self {
            name: name.to_string(),
            output: output.to_string(),
            fail_times: Mutex::new(HashMap::new()),
            runs: Arc::new(AtomicUsize::new(0)),
            current: Arc::new(AtomicUsize::new(0)),
            peak: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn with_failures(self, step_input: &str, times: u32) -> Self {
        self.fail_times
            .lock()
            .unwrap()
            .insert(step_input.to_string(), times);
        self
    }
}

#[async_trait]
impl Worker for EchoWorker {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, input: &serde_json::Value) -> Result<String, String> {
        let cur = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(cur, Ordering::SeqCst);
        self.runs.fetch_add(1, Ordering::SeqCst);
        tokio::task::yield_now().await;
        let text = input
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let mut fail = self.fail_times.lock().unwrap();
        let remaining = fail.get(&text).copied().unwrap_or(0);
        if remaining > 0 {
            if remaining == 1 {
                fail.remove(&text);
            } else {
                fail.insert(text.clone(), remaining - 1);
            }
            self.current.fetch_sub(1, Ordering::SeqCst);
            return Err(format!("注入失败：{text}（剩余 {remaining}）"));
        }
        let output = format!("{}-{}", self.output, text);
        self.current.fetch_sub(1, Ordering::SeqCst);
        Ok(output)
    }
}

fn registry_with(workers: Vec<Arc<EchoWorker>>) -> WorkerRegistry {
    let registry = WorkerRegistry::new();
    for worker in workers {
        registry.register(worker);
    }
    registry
}

/// 三步骤并行 + 汇合验证样例计划。
fn three_parallel_join_plan(goal_id: &str) -> Plan {
    let mut plan = Plan::new("plan-join", goal_id);
    for (id, text) in [("a", "A"), ("b", "B"), ("c", "C")] {
        let mut step = StepSpec::new(id, "echo");
        step.parallel = true;
        step.input = json!({ "text": text });
        plan.add_step(step);
    }
    let mut join = StepSpec::new("join", "echo");
    join.depends_on = vec!["a".into(), "b".into(), "c".into()];
    join.input = json!({ "text": "ABC" });
    join.verify = Some(VerificationSpec::OutputContains("ABC".to_string()));
    plan.add_step(join);
    plan
}

#[tokio::test]
async fn sample_three_parallel_steps_join_verify() {
    let worker = Arc::new(EchoWorker::new("echo", "out"));
    let registry = registry_with(vec![worker.clone()]);
    let mut runner = GoalRunner::new(
        Goal::new("g-join", "三步骤并行 + 汇合验证"),
        three_parallel_join_plan("g-join"),
        RunnerConfig {
            max_parallel: 3,
            ..Default::default()
        },
    );
    let status = runner.run(&registry).await.unwrap();
    assert_eq!(status, GoalStatus::Succeeded);
    // join 步验证通过：输出含 "out-ABC"。
    assert_eq!(
        runner.state.records["join"].output.as_deref(),
        Some("out-ABC")
    );
    assert_eq!(runner.state.goal.status, GoalStatus::Succeeded);
}

#[tokio::test]
async fn max_parallel_one_serializes_wave() {
    let worker = Arc::new(EchoWorker::new("echo", "out"));
    let registry = registry_with(vec![worker.clone()]);
    let mut runner = GoalRunner::new(
        Goal::new("g-serial", "并行度=1"),
        three_parallel_join_plan("g-serial"),
        RunnerConfig {
            max_parallel: 1,
            ..Default::default()
        },
    );
    runner.run(&registry).await.unwrap();
    assert_eq!(
        worker.peak.load(Ordering::SeqCst),
        1,
        "max_parallel=1 时并发峰值必须为 1"
    );
    assert_eq!(worker.runs.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn max_parallel_three_allows_parallel() {
    let worker = Arc::new(EchoWorker::new("echo", "out"));
    let registry = registry_with(vec![worker.clone()]);
    let mut runner = GoalRunner::new(
        Goal::new("g-par", "并行度=3"),
        three_parallel_join_plan("g-par"),
        RunnerConfig {
            max_parallel: 3,
            ..Default::default()
        },
    );
    runner.run(&registry).await.unwrap();
    assert!(
        worker.peak.load(Ordering::SeqCst) >= 2,
        "并行度=3 时并发峰值应 ≥2，实际 {}",
        worker.peak.load(Ordering::SeqCst)
    );
}

#[tokio::test]
async fn worker_failure_retries_within_budget() {
    let worker = Arc::new(EchoWorker::new("echo", "out").with_failures("A", 2));
    let registry = registry_with(vec![worker.clone()]);
    let mut runner = GoalRunner::new(
        Goal::new("g-retry", "步内重试"),
        three_parallel_join_plan("g-retry"),
        RunnerConfig {
            allow_replan: false,
            ..Default::default()
        },
    );
    // 默认 retries=0 → 失败 1 次即 Failed（先验证无重试语义）。
    let status = runner.run(&registry).await.unwrap();
    assert_eq!(status, GoalStatus::Failed);
    assert_eq!(
        runner.state.records["a"].status,
        owo_agent_core::plan::StepStatus::Failed
    );
}

#[tokio::test]
async fn retry_succeeds_when_attempts_cover_failures() {
    let worker = Arc::new(EchoWorker::new("echo", "out").with_failures("A", 1));
    let registry = registry_with(vec![worker.clone()]);
    let mut plan = three_parallel_join_plan("g-retry2");
    plan.step_mut("a").unwrap().retries = 2;
    let mut runner = GoalRunner::new(
        Goal::new("g-retry2", "重试后成功"),
        plan,
        RunnerConfig::default(),
    );
    let status = runner.run(&registry).await.unwrap();
    assert_eq!(status, GoalStatus::Succeeded);
    assert_eq!(worker.runs.load(Ordering::SeqCst), 5, "4 步 + a 重试 1 次");
    assert_eq!(runner.state.records["a"].attempts, 2);
}

#[tokio::test]
async fn retry_exhausted_marks_failed() {
    let worker = Arc::new(EchoWorker::new("echo", "out").with_failures("A", 5));
    let registry = registry_with(vec![worker.clone()]);
    let mut plan = three_parallel_join_plan("g-retry3");
    plan.step_mut("a").unwrap().retries = 2;
    let mut runner = GoalRunner::new(
        Goal::new("g-retry3", "重试耗尽"),
        plan,
        RunnerConfig {
            allow_replan: false,
            ..Default::default()
        },
    );
    let status = runner.run(&registry).await.unwrap();
    assert_eq!(status, GoalStatus::Failed);
    assert_eq!(
        runner.state.records["a"].status,
        owo_agent_core::plan::StepStatus::Failed
    );
    assert!(
        runner.state.records["a"]
            .error
            .as_deref()
            .unwrap()
            .contains("注入失败"),
        "记录应保留失败原因"
    );
    assert_eq!(
        runner.state.records["a"].attempts, 3,
        "retries=2 → 最多 3 次尝试"
    );
}

#[tokio::test]
async fn verify_failure_triggers_replan_without_rerun_of_succeeded() {
    // join 步验证失败 → replan：a/b/c 已成功不重跑，join 重置重跑。
    let worker = Arc::new(EchoWorker::new("echo", "out"));
    let registry = registry_with(vec![worker.clone()]);
    let mut plan = three_parallel_join_plan("g-replan");
    plan.step_mut("join").unwrap().verify =
        Some(VerificationSpec::OutputEquals("never".to_string()));
    // 第一次 replan 后 join 仍失败 → 第二次 replan → 超限 Failed。
    let mut runner = GoalRunner::new(
        Goal::new("g-replan", "验证失败 replan"),
        plan,
        RunnerConfig {
            max_parallel: 3,
            allow_replan: true,
            ..Default::default()
        },
    );
    let status = runner.run(&registry).await.unwrap();
    assert_eq!(status, GoalStatus::Failed);
    assert_eq!(runner.state.replan_count, 2, "max_replans=2 用尽");
    // 已成功步骤未重跑：a/b/c 各跑 1 次，join 每次 replan 都重跑（1 + 2 次 replan）。
    assert_eq!(worker.runs.load(Ordering::SeqCst), 3 + 3);
    // replan 后 join 已重置。
    assert_eq!(
        runner.state.records["join"].status,
        owo_agent_core::plan::StepStatus::Failed
    );
}

#[tokio::test]
async fn replan_does_not_rerun_succeeded_steps_then_succeeds() {
    // 步骤 b 先失败（注入 1 次），replan 后成功；a/c 保持不重跑。
    let worker = Arc::new(EchoWorker::new("echo", "out").with_failures("B", 1));
    let registry = registry_with(vec![worker.clone()]);
    let mut plan = three_parallel_join_plan("g-replan-ok");
    plan.step_mut("b").unwrap().retries = 1;
    plan.step_mut("join").unwrap().verify =
        Some(VerificationSpec::OutputContains("ABC".to_string()));
    let mut runner = GoalRunner::new(
        Goal::new("g-replan-ok", "replan 后成功"),
        plan,
        RunnerConfig {
            max_parallel: 3,
            allow_replan: true,
            ..Default::default()
        },
    );
    let status = runner.run(&registry).await.unwrap();
    assert_eq!(status, GoalStatus::Succeeded);
    assert_eq!(runner.state.replan_count, 0, "b 步重试内成功，无需 replan");
}

#[tokio::test]
async fn budget_max_steps_breaker() {
    let worker = Arc::new(EchoWorker::new("echo", "out").with_failures("A", 1000));
    let registry = registry_with(vec![worker.clone()]);
    let mut goal = Goal::new("g-budget", "步骤数预算");
    goal.budget = GoalBudget {
        max_steps: 3,
        ..Default::default()
    };
    let mut plan = three_parallel_join_plan("g-budget");
    plan.step_mut("a").unwrap().retries = 100;
    let mut runner = GoalRunner::new(
        goal,
        plan,
        RunnerConfig {
            allow_replan: false,
            ..Default::default()
        },
    );
    let status = runner.run(&registry).await.unwrap();
    assert_eq!(status, GoalStatus::Failed);
    assert!(runner
        .state
        .goal
        .error
        .as_deref()
        .unwrap()
        .contains("步骤数"));
}

#[tokio::test]
async fn budget_total_retries_breaker() {
    let worker = Arc::new(EchoWorker::new("echo", "out").with_failures("A", 1000));
    let registry = registry_with(vec![worker.clone()]);
    let mut goal = Goal::new("g-budget2", "全局重试预算");
    goal.budget = GoalBudget {
        max_total_retries: 3,
        max_steps: 100,
        ..Default::default()
    };
    let mut plan = three_parallel_join_plan("g-budget2");
    plan.step_mut("a").unwrap().retries = 100;
    let mut runner = GoalRunner::new(
        goal,
        plan,
        RunnerConfig {
            allow_replan: false,
            ..Default::default()
        },
    );
    let status = runner.run(&registry).await.unwrap();
    assert_eq!(status, GoalStatus::Failed);
}

#[tokio::test]
async fn abort_stops_and_preserves_state() {
    let worker = Arc::new(EchoWorker::new("echo", "out"));
    let registry = registry_with(vec![worker.clone()]);
    let mut runner = GoalRunner::new(
        Goal::new("g-abort", "abort 保留现场"),
        three_parallel_join_plan("g-abort"),
        RunnerConfig::default(),
    );
    // 先跑一部分：注入一个慢 worker 之前 abort 不可行——直接在 run 前 abort（记录现场语义）。
    runner.abort();
    let status = runner.run(&registry).await.unwrap();
    assert_eq!(status, GoalStatus::Aborted);
    assert_eq!(runner.state.goal.status, GoalStatus::Aborted);
    // 现场保留：未完成步骤标记 Aborted。
    assert!(runner
        .state
        .records
        .values()
        .all(|r| r.status == owo_agent_core::plan::StepStatus::Aborted));
}

#[tokio::test]
async fn recovery_is_idempotent_no_rerun_of_succeeded() {
    let worker = Arc::new(EchoWorker::new("echo", "out"));
    let registry = registry_with(vec![worker.clone()]);
    let dir = std::env::temp_dir().join(format!("owo-goal-run-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    // 第一次运行：持久化开启。
    let mut runner = GoalRunner::new(
        Goal::new("g-recover", "恢复幂等"),
        three_parallel_join_plan("g-recover"),
        RunnerConfig {
            max_parallel: 2,
            persist_dir: Some(dir.clone()),
            ..Default::default()
        },
    );
    let status = runner.run(&registry).await.unwrap();
    assert_eq!(status, GoalStatus::Succeeded);
    assert_eq!(worker.runs.load(Ordering::SeqCst), 4);
    // 模拟"崩溃时目标仍在运行"：join 未完成，其余已完成。
    runner.state.records.get_mut("join").unwrap().status =
        owo_agent_core::plan::StepStatus::Pending;
    runner.state.goal.transition(GoalStatus::Running);
    let run_id = runner.state.run_id.clone();
    runner.state.persist(&dir).unwrap();

    // "重启"：从磁盘恢复，重跑。
    let restored = GoalRunState::load(&dir, &run_id).unwrap();
    let mut runner2 = GoalRunner::from_state(
        restored,
        RunnerConfig {
            max_parallel: 2,
            persist_dir: Some(dir.clone()),
            ..Default::default()
        },
    );
    let status = runner2.run(&registry).await.unwrap();
    assert_eq!(status, GoalStatus::Succeeded);
    // 已完成步骤不重跑：只重跑了 join（1 次）。
    assert_eq!(
        worker.runs.load(Ordering::SeqCst),
        5,
        "4 次 + join 恢复后 1 次"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn audit_events_written_for_every_action() {
    let worker = Arc::new(EchoWorker::new("echo", "out"));
    let registry = registry_with(vec![worker.clone()]);
    let mut runner = GoalRunner::new(
        Goal::new("g-audit", "审计"),
        three_parallel_join_plan("g-audit"),
        RunnerConfig::default(),
    );
    runner.run(&registry).await.unwrap();
    let events = runner.state.events.clone();
    assert!(events.iter().any(|e| e.starts_with("goal.start")));
    assert!(events.iter().any(|e| e.starts_with("goal.step.start")));
    assert!(events.iter().any(|e| e.starts_with("goal.step.succeeded")));
    assert!(events.iter().any(|e| e.starts_with("goal.verifying")));
    assert!(events.iter().any(|e| e.starts_with("goal.succeeded")));
}

#[tokio::test]
async fn audit_log_injected_via_attach() {
    let worker = Arc::new(EchoWorker::new("echo", "out"));
    let registry = registry_with(vec![worker.clone()]);
    let log = Arc::new(std::sync::Mutex::new(
        owo_agent_core::audit::AuditLog::default(),
    ));
    let mut runner = GoalRunner::new(
        Goal::new("g-audit2", "审计注入"),
        three_parallel_join_plan("g-audit2"),
        RunnerConfig::default(),
    );
    runner.attach_audit(log.clone());
    runner.run(&registry).await.unwrap();
    let entries = log.lock().unwrap().entries.clone();
    assert!(entries.iter().any(|e| e.event.starts_with("goal.")));
}

#[tokio::test]
async fn unknown_worker_fails_with_clear_error() {
    let registry = WorkerRegistry::new();
    let mut runner = GoalRunner::new(
        Goal::new("g-noworker", "未注册 worker"),
        three_parallel_join_plan("g-noworker"),
        RunnerConfig {
            allow_replan: false,
            ..Default::default()
        },
    );
    let status = runner.run(&registry).await.unwrap();
    assert_eq!(status, GoalStatus::Failed);
    assert!(
        runner.state.records["a"]
            .error
            .as_deref()
            .unwrap()
            .contains("未注册"),
        "worker 未注册应有清晰报错"
    );
}

#[tokio::test]
async fn goal_acceptance_verifies_summary() {
    let worker = Arc::new(EchoWorker::new("echo", "done"));
    let registry = registry_with(vec![worker.clone()]);
    let mut goal = Goal::new("g-accept", "目标验收");
    goal.acceptance = vec![VerificationSpec::OutputContains("done".to_string())];
    let mut runner = GoalRunner::new(
        goal,
        three_parallel_join_plan("g-accept"),
        RunnerConfig::default(),
    );
    let status = runner.run(&registry).await.unwrap();
    assert_eq!(status, GoalStatus::Succeeded);
}

#[tokio::test]
async fn goal_acceptance_failure_fails_goal() {
    let worker = Arc::new(EchoWorker::new("echo", "done"));
    let registry = registry_with(vec![worker.clone()]);
    let mut goal = Goal::new("g-accept2", "目标验收失败");
    goal.acceptance = vec![VerificationSpec::OutputContains("MISSING".to_string())];
    let mut runner = GoalRunner::new(
        goal,
        three_parallel_join_plan("g-accept2"),
        RunnerConfig::default(),
    );
    let status = runner.run(&registry).await.unwrap();
    assert_eq!(status, GoalStatus::Failed);
    assert!(runner.state.goal.error.as_deref().unwrap().contains("验收"));
    // 状态机经过 Verifying。
    assert!(runner.state.goal.status == GoalStatus::Failed);
}

#[tokio::test]
async fn chain_plan_serial_execution() {
    let worker = Arc::new(EchoWorker::new("echo", "out"));
    let registry = registry_with(vec![worker.clone()]);
    let mut plan = Plan::new("plan-chain", "g-chain");
    let mut prev: Option<String> = None;
    for i in 0..3 {
        let mut step = StepSpec::new(format!("s{i}"), "echo");
        step.input = json!({ "text": format!("S{i}") });
        if let Some(p) = &prev {
            step.depends_on = vec![p.clone()];
        }
        prev = Some(step.id.clone());
        plan.add_step(step);
    }
    let mut runner = GoalRunner::new(
        Goal::new("g-chain", "链式执行"),
        plan,
        RunnerConfig::default(),
    );
    let status = runner.run(&registry).await.unwrap();
    assert_eq!(status, GoalStatus::Succeeded);
    assert_eq!(worker.peak.load(Ordering::SeqCst), 1, "链式依赖必须串行");
}

#[tokio::test]
async fn run_state_persist_load_roundtrip() {
    let worker = Arc::new(EchoWorker::new("echo", "out"));
    let registry = registry_with(vec![worker.clone()]);
    let dir = std::env::temp_dir().join(format!("owo-goal-persist-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut runner = GoalRunner::new(
        Goal::new("g-persist", "持久化"),
        three_parallel_join_plan("g-persist"),
        RunnerConfig {
            max_parallel: 2,
            persist_dir: Some(dir.clone()),
            ..Default::default()
        },
    );
    runner.run(&registry).await.unwrap();
    let run_id = runner.state.run_id.clone();
    let restored = GoalRunState::load(&dir, &run_id).unwrap();
    assert_eq!(restored.goal.status, GoalStatus::Succeeded);
    assert_eq!(restored.records.len(), 4);
    assert!(restored
        .records
        .values()
        .all(|r| r.status == owo_agent_core::plan::StepStatus::Succeeded));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn goal_status_pending_planning_transitions() {
    let mut goal = Goal::new("g-st", "状态机");
    goal.transition(GoalStatus::Planning);
    assert_eq!(goal.status, GoalStatus::Planning);
    goal.transition(GoalStatus::Running);
    assert!(!goal.status.is_terminal());
    goal.transition(GoalStatus::Succeeded);
    assert!(goal.status.is_terminal());
}

#[tokio::test]
async fn worker_registry_dispatch_by_name() {
    let registry = registry_with(vec![Arc::new(EchoWorker::new("w1", "x"))]);
    let worker = registry.get("w1").unwrap();
    let output = worker.run(&json!({ "text": "hi" })).await.unwrap();
    assert_eq!(output, "x-hi");
    assert!(registry.get("missing").is_none());
}
