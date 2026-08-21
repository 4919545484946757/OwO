//! §12 Goal/Plan 多 Agent 编排契约测试（R8 简化版：主链路 + R8 新增接线）。
//!
//! 覆盖：三步骤并行+汇合验证、步内重试、replan 不重跑已成功步骤、abort 保留现场、
//! 恢复幂等、WorkerPool 子进程执行、未知 worker、R8：abort 取消传播、
//! ExperienceStore 幂等写入、CapabilityCard 能力路由（拒绝/选中）。

use async_trait::async_trait;
use owo_agent_core::capability::{CapabilityCard, CapabilityWorkerRegistry, Os, WorkerRequirement};
use owo_agent_core::experience_store::ExperienceStore;
use owo_agent_core::fleet::{RestartPolicy, RestartRule};
use owo_agent_core::goal::{
    Goal, GoalRunState, GoalRunner, GoalStatus, RunnerConfig, Worker, WorkerRegistry,
};
use owo_agent_core::plan::{Plan, StepSpec, VerificationSpec};
use owo_agent_core::worker_pool::{child, WorkerPool, WorkerSpec};
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

/// 记录 worker：输出=input.text + 计数；可注入失败次数。
struct EchoWorker {
    name: String,
    output: String,
    fail_times: Mutex<HashMap<String, u32>>,
    runs: Arc<AtomicUsize>,
}

impl EchoWorker {
    fn new(name: &str, output: &str) -> Self {
        Self {
            name: name.to_string(),
            output: output.to_string(),
            fail_times: Mutex::new(HashMap::new()),
            runs: Arc::new(AtomicUsize::new(0)),
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
            return Err(format!("注入失败：{text}（剩余 {remaining}）"));
        }
        Ok(format!("{}-{}", self.output, text))
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

fn single_step_plan(goal_id: &str, input: serde_json::Value) -> Plan {
    let mut plan = Plan::new("plan-single", goal_id);
    let mut step = StepSpec::new("a", "echo");
    step.input = input;
    plan.add_step(step);
    plan
}

/// 子进程模式入口（父进程用 `--exact goal_child_entry --nocapture` + 环境标记拉起）。
#[test]
fn goal_child_entry() {
    if std::env::var("OWO_WORKER_CHILD").is_err() {
        return; // 父进程测试模式下直接返回
    }
    static FAILS: LazyLock<Mutex<HashMap<String, u32>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    child::run_child_protocol(|input| {
        if input
            .get("crash")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            std::process::exit(42);
        }
        if let Some(ms) = input.get("sleep_ms").and_then(|v| v.as_u64()) {
            std::thread::sleep(Duration::from_millis(ms));
        }
        let text = input
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let mut fails = FAILS.lock().unwrap();
        if input
            .get("fail_first")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            && !fails.contains_key(&text)
        {
            fails.insert(text.clone(), 1);
        }
        let remaining = fails.get(&text).copied().unwrap_or(0);
        if remaining > 0 {
            if remaining == 1 {
                fails.remove(&text);
            } else {
                fails.insert(text.clone(), remaining - 1);
            }
            drop(fails);
            return Err(format!("注入失败：{text}（剩余 {remaining}）"));
        }
        drop(fails);
        Ok(format!("out-{text}"))
    });
}

fn pool_echo_spec(id: &str) -> WorkerSpec {
    WorkerSpec::new(id, std::env::current_exe().unwrap())
        .args(vec![
            "--exact".to_string(),
            "goal_child_entry".to_string(),
            "--nocapture".to_string(),
            "--quiet".to_string(),
        ])
        .env_whitelist(vec![("OWO_WORKER_CHILD".to_string(), "1".to_string())])
        .restart_rule(RestartRule {
            max_restarts: 2,
            base_backoff_secs: 0,
            policy: RestartPolicy::OneForOne,
        })
}

fn pool_config(pool: &WorkerPool) -> RunnerConfig {
    RunnerConfig {
        use_worker_pool: true,
        worker_pool: Some(pool.clone()),
        ..Default::default()
    }
}

// ---------- 主链路 ----------

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
    assert_eq!(
        runner.state.records["join"].output.as_deref(),
        Some("out-ABC")
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
async fn verify_failure_triggers_replan_without_rerun_of_succeeded() {
    let worker = Arc::new(EchoWorker::new("echo", "out"));
    let registry = registry_with(vec![worker.clone()]);
    let mut plan = three_parallel_join_plan("g-replan");
    plan.step_mut("join").unwrap().verify =
        Some(VerificationSpec::OutputEquals("never".to_string()));
    let mut runner = GoalRunner::new(
        Goal::new("g-replan", "验证失败 replan"),
        plan,
        RunnerConfig {
            max_parallel: 3,
            ..Default::default()
        },
    );
    let status = runner.run(&registry).await.unwrap();
    assert_eq!(status, GoalStatus::Failed);
    assert_eq!(runner.state.replan_count, 2, "max_replans=2 用尽");
    assert_eq!(
        worker.runs.load(Ordering::SeqCst),
        3 + 3,
        "a/b/c 不重跑，join 每次 replan 重跑"
    );
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
    runner.abort();
    let status = runner.run(&registry).await.unwrap();
    assert_eq!(status, GoalStatus::Aborted);
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
    // 模拟"崩溃时 join 未完成"：恢复后只重跑 join。
    runner.state.records.get_mut("join").unwrap().status =
        owo_agent_core::plan::StepStatus::Pending;
    runner.state.goal.transition(GoalStatus::Running);
    let run_id = runner.state.run_id.clone();
    runner.state.persist(&dir).unwrap();

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
    assert_eq!(
        worker.runs.load(Ordering::SeqCst),
        5,
        "4 次 + join 恢复后 1 次"
    );
    let _ = std::fs::remove_dir_all(&dir);
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

// ---------- WorkerPool 子进程路径 ----------

#[tokio::test]
async fn pool_steps_execute_when_feature_flag_on() {
    let pool = WorkerPool::new();
    pool.spawn(pool_echo_spec("echo")).await.unwrap();
    let registry = WorkerRegistry::new(); // 进程内 registry 为空：步骤必须走子进程。
    let mut plan = Plan::new("plan-pool", "g-pool");
    let mut a = StepSpec::new("a", "echo");
    a.input = json!({ "text": "A" });
    let mut b = StepSpec::new("b", "echo");
    b.input = json!({ "text": "B" });
    b.depends_on = vec!["a".into()];
    plan.add_step(a);
    plan.add_step(b);
    let mut runner = GoalRunner::new(
        Goal::new("g-pool", "子进程 worker"),
        plan,
        pool_config(&pool),
    );
    let status = runner.run(&registry).await.unwrap();
    if status != GoalStatus::Succeeded {
        panic!(
            "pool 步骤失败：status={status:?} error={:?} a={:?} b={:?}",
            runner.state.goal.error,
            runner.state.records["a"].error,
            runner.state.records["b"].error
        );
    }
    assert_eq!(runner.state.records["a"].output.as_deref(), Some("out-A"));
    assert_eq!(runner.state.records["b"].output.as_deref(), Some("out-B"));
    pool.shutdown().await;
}

// ---------- R8：ExperienceStore 接线 ----------

#[tokio::test]
async fn experience_store_records_step_and_goal_outcomes() {
    let store = ExperienceStore::in_memory();
    let worker = Arc::new(EchoWorker::new("echo", "out"));
    let registry = registry_with(vec![worker.clone()]);
    let mut runner = GoalRunner::new(
        Goal::new("g-exp", "经验写入"),
        three_parallel_join_plan("g-exp"),
        RunnerConfig::default(),
    );
    runner.attach_experience(store.clone());
    let status = runner.run(&registry).await.unwrap();
    assert_eq!(status, GoalStatus::Succeeded);
    let events = store.events();
    // 4 个步骤 + 1 个 Goal 事件。
    let worker_events = events
        .iter()
        .filter(|e| e.kind == owo_agent_core::experience_store::ExperienceKind::WorkerTask)
        .count();
    let goal_events = events
        .iter()
        .filter(|e| e.kind == owo_agent_core::experience_store::ExperienceKind::GoalRun)
        .count();
    assert_eq!(worker_events, 4, "每步一条（幂等键 run_id:step_id）");
    assert_eq!(goal_events, 1, "Goal 一条（键 run_id）");
    // 空闲聚合：worker 成功率 100%。
    let insights = store.aggregate();
    assert_eq!(insights.len(), 1);
    assert_eq!(insights[0].worker, "echo");
    assert_eq!(insights[0].successes, 4);
    assert!((insights[0].anchor_prior - 1.0).abs() < 1e-9);
}

// ---------- R8：CapabilityCard 能力路由 ----------

#[tokio::test]
async fn capability_route_picks_pool_worker_when_requirement_met() {
    let pool = WorkerPool::new();
    pool.spawn(pool_echo_spec("w-cap")).await.unwrap();
    let registry = WorkerRegistry::new();
    let capabilities = CapabilityWorkerRegistry::new();
    capabilities.register(CapabilityCard::new("w-cap").actions(vec!["shell".to_string()]));
    let mut runner = GoalRunner::new(
        Goal::new("g-cap-ok", "能力路由选中"),
        single_step_plan(
            "g-cap-ok",
            json!({
                "text": "A",
                "_cap": serde_json::to_value(WorkerRequirement {
                    actions: vec!["shell".to_string()],
                    ..Default::default()
                })
                .unwrap()
            }),
        ),
        RunnerConfig {
            use_worker_pool: true,
            worker_pool: Some(pool.clone()),
            capability_registry: Some(capabilities),
            allow_replan: false,
            ..Default::default()
        },
    );
    let status = runner.run(&registry).await.unwrap();
    assert_eq!(status, GoalStatus::Succeeded);
    assert_eq!(
        runner.state.records["a"].output.as_deref(),
        Some("out-A"),
        "步骤应经能力路由选中 w-cap 子进程执行"
    );
    pool.shutdown().await;
}

#[tokio::test]
async fn capability_requirement_unmet_rejects_explicitly() {
    let pool = WorkerPool::new();
    pool.spawn(pool_echo_spec("w-cap")).await.unwrap();
    let registry = WorkerRegistry::new();
    let capabilities = CapabilityWorkerRegistry::new();
    capabilities.register(CapabilityCard::new("w-cap").actions(vec!["shell".to_string()]));
    let mut runner = GoalRunner::new(
        Goal::new("g-cap-reject", "能力不满足拒绝"),
        single_step_plan(
            "g-cap-reject",
            json!({
                "text": "A",
                "_cap": serde_json::to_value(WorkerRequirement {
                    // 硬约束不满足（本机为 Windows，要求 Linux）：必须显式拒绝。
                    os: Some(Os::Linux),
                    ..Default::default()
                })
                .unwrap()
            }),
        ),
        RunnerConfig {
            use_worker_pool: true,
            worker_pool: Some(pool.clone()),
            capability_registry: Some(capabilities),
            allow_replan: false,
            ..Default::default()
        },
    );
    let status = runner.run(&registry).await.unwrap();
    if status != GoalStatus::Failed {
        panic!(
            "能力不满足应失败：status={status:?} error={:?} step={:?} output={:?}",
            runner.state.goal.error,
            runner.state.records["a"].error,
            runner.state.records["a"].output
        );
    }
    let error = runner.state.records["a"].error.as_deref().unwrap();
    assert!(
        error.contains("能力不满足") || error.contains("能力"),
        "需求不满足必须显式拒绝：{error}"
    );
    pool.shutdown().await;
}

// ---------- R9：聚合执行器 + 能力注册表持久化 ----------

#[tokio::test]
async fn aggregation_report_persists_and_replays() {
    use owo_agent_core::experience_store::{
        load_aggregation_report, Attribution, ExperienceKind, ExperienceStore, Outcome,
    };
    let store = ExperienceStore::in_memory();
    for i in 0..3 {
        store
            .record_worker_outcome(
                format!("agg-c{i}"),
                "w1",
                Outcome::Success,
                Attribution {
                    goal_id: Some("g1".to_string()),
                    plan_id: None,
                    step_id: Some("s1".to_string()),
                    input_keys: vec!["text".to_string(), "mode".to_string()],
                    error: None,
                },
            )
            .unwrap();
    }
    store
        .record_worker_outcome(
            "agg-c-fail",
            "w1",
            Outcome::Failure,
            Attribution {
                goal_id: Some("g1".to_string()),
                plan_id: None,
                step_id: Some("s2".to_string()),
                input_keys: vec!["text".to_string()],
                error: Some("验证失败：输出不含关键字".to_string()),
            },
        )
        .unwrap();
    let dir = std::env::temp_dir().join(format!("owo-agg-{}", std::process::id()));
    // 聚合执行器：蒸馏技能元数据更新并落盘报告。
    let report = store.run_aggregation(&dir).unwrap();
    assert_eq!(report.event_count, 4);
    assert_eq!(report.insights.len(), 1);
    let insight = &report.insights[0];
    assert_eq!(insight.worker, "w1");
    assert!(insight
        .suggested_preconditions
        .contains(&"mode".to_string()));
    assert!(insight.suggested_assertions[0].contains("输出不含关键字"));
    assert!((insight.anchor_prior - 0.75).abs() < 1e-9);
    // 崩溃重放：读回报告一致（重跑结果幂等）。
    let restored = load_aggregation_report(&dir).unwrap();
    assert_eq!(restored.insights, report.insights);
    assert_eq!(restored.event_count, 4);
    // 事件种类区分（GoalRun 不参与技能蒸馏）。
    assert!(store
        .events()
        .iter()
        .any(|e| e.kind == ExperienceKind::WorkerTask));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn capability_registry_persist_health_and_stats() {
    use owo_agent_core::capability::{CapabilityCard, CapabilityWorkerRegistry, WorkerRequirement};
    let dir = std::env::temp_dir().join(format!("owo-cap-{}", std::process::id()));
    let path = dir.join("capabilities.json");
    let reg = CapabilityWorkerRegistry::new();
    reg.register(CapabilityCard::new("w1").actions(vec!["shell".to_string()]));
    reg.register(
        CapabilityCard::new("w2").actions(vec!["shell".to_string(), "browser".to_string()]),
    );
    // 命中率统计。
    assert_eq!(
        reg.route(&WorkerRequirement {
            actions: vec!["shell".to_string()],
            ..Default::default()
        }),
        owo_agent_core::capability::RouteDecision::Pick("w1".to_string())
    );
    assert_eq!(
        reg.route(&WorkerRequirement {
            actions: vec!["browser".to_string()],
            ..Default::default()
        }),
        owo_agent_core::capability::RouteDecision::Pick("w2".to_string())
    );
    let stats = reg.route_stats();
    assert_eq!(stats.picks, 2);
    assert_eq!(stats.total(), 2);
    // 健康度：w2 持续失败 → 路由跳过 w2；w1 可降级执行（显式降级，不静默）。
    for _ in 0..4 {
        reg.mark_health("w2", false);
    }
    reg.mark_health("w1", true);
    assert_eq!(
        reg.worker_health("w2").unwrap().success_rate(),
        0.0,
        "w2 失败率 100%"
    );
    match reg.route(&WorkerRequirement {
        actions: vec!["browser".to_string()],
        ..Default::default()
    }) {
        owo_agent_core::capability::RouteDecision::Degrade { worker, missing } => {
            assert_eq!(worker, "w1", "健康度不足的 w2 被跳过，w1 显式降级");
            assert!(missing.contains(&"action:browser".to_string()));
        }
        other => panic!("应显式降级到健康 worker：{other:?}"),
    }
    // 持久化 → 加载：快照完整恢复（含健康度与统计）。
    reg.persist(&path).unwrap();
    let restored = CapabilityWorkerRegistry::load(&path).unwrap();
    assert_eq!(restored.len(), 2);
    assert_eq!(restored.worker_health("w2").unwrap().failures, 4);
    assert_eq!(restored.route_stats().picks, 2, "命中率随快照恢复");
    assert_eq!(restored.route_stats().degrades, 1);
    assert_eq!(restored.route_stats().rejects, 0);
    let _ = std::fs::remove_dir_all(&dir);
}
