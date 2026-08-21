//! 多 Agent P0 编排契约测试（fleet 层）。
//!
//! 覆盖：总线背压（可合并丢弃 + 关键保留）、correlation_id 去重、fan-out 超时/取消传播/
//! 部分成功仲裁（已成功保留 + 失败单独重试）、等待图环检测与优先级仲裁、消息 id 单调。

use owo_agent_core::bus_store::BusStore;
use owo_agent_core::fleet::{
    arbitrate_wait_cycle, dedupe_messages, detect_wait_cycle, fan_out_cfg, message_dedup_key,
    AgentBus, Budget, BusMessage, FanOutConfig, FanOutStatus, MessageKind, OverflowPolicy,
    WaitEdge, WaitGraph,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

// ---------- 总线：背压与去重 ----------

#[tokio::test]
async fn bus_overflow_drops_mergeable_keeps_critical() {
    let bus = AgentBus::new();
    bus.register("worker-a", 2).await;
    bus.send(
        "core",
        "worker-a",
        MessageKind::Task,
        "corr-1",
        serde_json::json!({"q": 1}),
        OverflowPolicy::DropMergeable,
    )
    .await
    .unwrap();
    bus.send(
        "core",
        "worker-a",
        MessageKind::Progress,
        "corr-1",
        serde_json::json!({"p": 1}),
        OverflowPolicy::DropMergeable,
    )
    .await
    .unwrap();
    // 邮箱已满：进度类（可合并）静默丢弃（send 仍返回消息 id，投递结果由 pending 反映）。
    assert!(bus
        .send(
            "core",
            "worker-a",
            MessageKind::Progress,
            "corr-1",
            serde_json::json!({"p": 2}),
            OverflowPolicy::DropMergeable,
        )
        .await
        .is_ok());
    // 关键事件（评审/任务/结果）在溢出时报满而非丢弃。
    assert!(bus
        .send(
            "core",
            "worker-a",
            MessageKind::Review,
            "corr-1",
            serde_json::json!({"r": 1}),
            OverflowPolicy::DropMergeable,
        )
        .await
        .is_err());
    assert_eq!(bus.pending("worker-a").await, 2, "关键事件不得被丢弃");
    // 消费后：保留 Task + 首条 Progress，第二条 Progress 被丢弃。
    let drained = bus.drain("worker-a").await;
    assert_eq!(drained.len(), 2);
    assert_eq!(drained[0].kind, MessageKind::Task);
    assert_eq!(drained[1].kind, MessageKind::Progress);
}

#[tokio::test]
async fn bus_message_ids_are_monotonic() {
    let bus = AgentBus::new();
    bus.register("w", 8).await;
    let mut prev: Option<u64> = None;
    for i in 0..5u64 {
        let id = bus
            .send(
                "core",
                "w",
                MessageKind::Task,
                format!("corr-{i}"),
                serde_json::Value::Null,
                OverflowPolicy::Reject,
            )
            .await
            .unwrap();
        if let Some(p) = prev {
            assert!(id > p, "消息 id 必须单调递增");
        }
        prev = Some(id);
    }
}

#[tokio::test]
async fn dedupe_detects_duplicates_by_correlation_and_payload() {
    let bus = AgentBus::new();
    bus.register("w", 16).await;
    // 同一 correlation_id + 同一载荷投递两次（at-least-once 模拟）。
    bus.send(
        "core",
        "w",
        MessageKind::Task,
        "corr-dup",
        serde_json::json!({"op": "inject"}),
        OverflowPolicy::Reject,
    )
    .await
    .unwrap();
    bus.send(
        "core",
        "w",
        MessageKind::Task,
        "corr-dup",
        serde_json::json!({"op": "inject"}),
        OverflowPolicy::Reject,
    )
    .await
    .unwrap();
    // 不同载荷 / 不同 correlation_id 不算重复。
    bus.send(
        "core",
        "w",
        MessageKind::Task,
        "corr-dup",
        serde_json::json!({"op": "other"}),
        OverflowPolicy::Reject,
    )
    .await
    .unwrap();
    bus.send(
        "core",
        "w",
        MessageKind::Task,
        "corr-other",
        serde_json::json!({"op": "inject"}),
        OverflowPolicy::Reject,
    )
    .await
    .unwrap();
    let messages = bus.drain("w").await;
    assert_eq!(messages.len(), 4);
    let deduped = dedupe_messages(&messages);
    assert_eq!(
        deduped.len(),
        3,
        "重复消息（同 correlation_id + 同载荷）应去重为 1 条"
    );
    // 去重键必须区分 correlation_id。
    assert_ne!(
        message_dedup_key(&messages[0]),
        message_dedup_key(&messages[3]),
        "correlation_id 不同必须产生不同去重键"
    );
}

// ---------- fan-out：超时 / 取消 / 部分成功仲裁 ----------

#[tokio::test]
async fn fan_out_per_worker_timeout_marks_timed_out_others_succeed() {
    let workers: Vec<String> = vec!["slow".into(), "fast1".into(), "fast2".into()];
    let report = fan_out_cfg(
        &workers,
        FanOutConfig {
            max_parallel: 3,
            per_worker_timeout: Some(Duration::from_millis(10)),
            ..Default::default()
        },
        "corr-timeout",
        |id| async move {
            if id == "slow" {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok("too-late".to_string())
            } else {
                Ok(format!("ok:{id}"))
            }
        },
    )
    .await;
    assert_eq!(report.correlation_id, "corr-timeout");
    assert_eq!(
        report.outcome("slow").unwrap().status,
        FanOutStatus::TimedOut
    );
    assert!(report
        .outcome("slow")
        .unwrap()
        .error
        .as_deref()
        .unwrap()
        .contains("timed out"));
    assert_eq!(
        report.outcome("fast1").unwrap().status,
        FanOutStatus::Succeeded
    );
    assert_eq!(
        report.outcome("fast2").unwrap().status,
        FanOutStatus::Succeeded
    );
    assert_eq!(report.retryable().len(), 1, "只有超时的 worker 可单独重试");
}

#[tokio::test]
async fn fan_out_cancellation_propagates_no_orphan_tasks() {
    let workers: Vec<String> = vec!["w1".into(), "w2".into(), "w3".into(), "w4".into()];
    let cancelled = Arc::new(AtomicBool::new(false));
    let runs = Arc::new(AtomicUsize::new(0));
    let runs_clone = Arc::clone(&runs);
    let cancelled_clone = Arc::clone(&cancelled);
    let workers_clone = workers.clone();
    let handle = tokio::spawn(async move {
        fan_out_cfg(
            &workers_clone,
            FanOutConfig {
                max_parallel: 2,
                cancelled: Some(cancelled_clone),
                ..Default::default()
            },
            "corr-cancel",
            move |id| {
                let runs = Arc::clone(&runs_clone);
                async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    Ok(format!("ok:{id}"))
                }
            },
        )
        .await
    });
    // 等 2 个在飞 worker 启动后取消：取消在调度循环边界生效——
    // 已完成者保留（部分成功），在飞者 abort 为 Cancelled，未启动者不再启动。
    tokio::time::sleep(Duration::from_millis(20)).await;
    cancelled.store(true, Ordering::SeqCst);
    let report = handle.await.expect("fan_out 任务不 panic");
    let cancelled_count = report
        .outcomes
        .iter()
        .filter(|o| o.status == FanOutStatus::Cancelled)
        .count();
    let succeeded_count = report
        .outcomes
        .iter()
        .filter(|o| o.status == FanOutStatus::Succeeded)
        .count();
    assert_eq!(
        succeeded_count, 1,
        "恰 1 个在飞 worker 在取消生效前完成：{:?}",
        report.outcomes
    );
    assert_eq!(
        cancelled_count, 3,
        "其余全部 Cancelled（无孤儿任务）：{:?}",
        report.outcomes
    );
    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "取消传播：不得再启动新 worker（无孤儿任务）"
    );
}

#[tokio::test]
async fn fan_out_partial_success_keeps_succeeded_retries_failed_only() {
    let workers: Vec<String> = vec!["w1".into(), "w2".into(), "w3".into()];
    let runs = Arc::new(AtomicUsize::new(0));
    let first_runs = Arc::clone(&runs);
    let first = fan_out_cfg(
        &workers,
        FanOutConfig {
            max_parallel: 3,
            ..Default::default()
        },
        "corr-partial",
        move |id| {
            let runs = Arc::clone(&first_runs);
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                if id == "w2" {
                    Err("boom".to_string())
                } else {
                    Ok(format!("ok:{id}"))
                }
            }
        },
    )
    .await;
    assert_eq!(first.succeeded().len(), 2, "已成功结果保留");
    assert_eq!(first.failed().len(), 1);
    let retryable: Vec<String> = first.retryable().iter().map(|o| o.worker.clone()).collect();
    assert_eq!(retryable, vec!["w2".to_string()]);
    // 仅单独重试失败子任务 w2（重试仍计入运行数）。
    let retry_runs = Arc::clone(&runs);
    let second = fan_out_cfg(
        &retryable,
        FanOutConfig::default(),
        "corr-partial-retry",
        move |id| {
            let runs = Arc::clone(&retry_runs);
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(format!("ok:{id}"))
            }
        },
    )
    .await;
    assert!(second.all_succeeded());
    assert_eq!(
        runs.load(Ordering::SeqCst),
        4,
        "w1/w3 不得重跑，仅 w2 重试 1 次"
    );
    assert_eq!(
        first.outcome("w1").unwrap().output.as_deref(),
        Some("ok:w1")
    );
    assert_eq!(
        first.outcome("w3").unwrap().output.as_deref(),
        Some("ok:w3")
    );
}

#[tokio::test]
async fn fan_out_report_orders_by_input_and_zero_budget_aborts() {
    let workers: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
    let report = fan_out_cfg(
        &workers,
        FanOutConfig {
            max_parallel: 1,
            budget: Budget {
                max_duration_secs: 0,
                ..Default::default()
            },
            ..Default::default()
        },
        "corr-order",
        |id| async move { Ok(format!("ok:{id}")) },
    )
    .await;
    let ids: Vec<&str> = report.outcomes.iter().map(|o| o.worker.as_str()).collect();
    assert_eq!(ids, vec!["a", "b", "c"], "结果必须按输入顺序返回");
    assert!(
        report
            .outcomes
            .iter()
            .all(|o| o.status == FanOutStatus::Aborted),
        "时长预算为 0 应立即整体硬停：{:?}",
        report.outcomes
    );
    assert!(!report.all_succeeded());
}

// ---------- 等待图：环检测与优先级仲裁 ----------

#[test]
fn wait_graph_detects_cycle_and_resolves_low_priority_cancel() {
    let mut graph = WaitGraph::new();
    graph.add("a", "b", None);
    graph.add("b", "c", Some(Duration::from_secs(30)));
    graph.add("c", "a", None);
    graph.set_priority("a", 1);
    graph.set_priority("b", 2);
    graph.set_priority("c", 3);
    let cycle = graph.cycle().expect("等待图存在环");
    assert!(cycle.len() >= 3, "环路径：{cycle:?}");
    assert_eq!(cycle.first(), cycle.last());
    let resolution = graph.resolve().expect("可仲裁");
    assert_eq!(resolution.cancel, "c", "c 优先级最低（3），应被取消");
    assert!(
        resolution.reason.contains("等待图死锁"),
        "{}",
        resolution.reason
    );
}

#[test]
fn wait_graph_clean_dag_has_no_cycle() {
    let mut graph = WaitGraph::new();
    graph.add("a", "b", None);
    graph.add("a", "c", None);
    graph.add("b", "d", Some(Duration::from_secs(10)));
    assert!(graph.cycle().is_none());
    assert!(graph.resolve().is_none());
}

#[test]
fn detect_wait_cycle_uses_waiter_waited_direction() {
    // 等待方向必须正确：c 等 a、a 等 b、b 等 c 构成环。
    let edges = vec![
        WaitEdge::new("c", "a"),
        WaitEdge::new("a", "b"),
        WaitEdge::new("b", "c"),
    ];
    assert!(detect_wait_cycle(&edges).is_some());
    // 真 DAG（无环等待拓扑）不得误报。
    let acyclic = vec![
        WaitEdge::new("a", "b"),
        WaitEdge::new("a", "c"),
        WaitEdge::new("b", "d"),
        WaitEdge::new("c", "d"),
    ];
    assert!(detect_wait_cycle(&acyclic).is_none());
}

#[test]
fn wait_cycle_arbitration_tie_is_deterministic_and_defaults_lowest() {
    // 并列优先级 → 字典序最大者（确定性）。
    let mut priority: HashMap<String, u32> = HashMap::new();
    priority.insert("a".to_string(), 1);
    priority.insert("b".to_string(), 1);
    priority.insert("c".to_string(), 1);
    let cycle = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    assert_eq!(arbitrate_wait_cycle(&cycle, &priority), "c");
    // 未声明优先级的 agent 视为最低优先级。
    priority.clear();
    priority.insert("a".to_string(), 0);
    assert_eq!(
        arbitrate_wait_cycle(&cycle, &priority),
        "c",
        "c 无优先级声明（默认最低）应被取消"
    );
    // 重复调用结果稳定。
    for _ in 0..5 {
        assert_eq!(arbitrate_wait_cycle(&cycle, &priority), "c");
    }
}

// ---------- 总线消息字段完整性 ----------

// ---------- R8：能力过滤与经验写入 ----------

use owo_agent_core::capability::{CapabilityCard, CapabilityWorkerRegistry, WorkerRequirement};
use owo_agent_core::experience_store::{ExperienceKind, ExperienceStore, Outcome};

#[tokio::test]
async fn fan_out_marks_unfit_workers_without_scheduling() {
    let workers: Vec<String> = vec!["fit".into(), "unfit".into()];
    let capabilities = CapabilityWorkerRegistry::new();
    capabilities.register(CapabilityCard::new("fit").actions(vec!["shell".into()]));
    capabilities.register(CapabilityCard::new("unfit").actions(vec!["browser".into()]));
    let runs = Arc::new(AtomicUsize::new(0));
    let runs_clone = Arc::clone(&runs);
    let report = fan_out_cfg(
        &workers,
        FanOutConfig {
            max_parallel: 2,
            capabilities: Some(capabilities),
            requirement: Some(WorkerRequirement {
                actions: vec!["shell".into()],
                ..Default::default()
            }),
            ..Default::default()
        },
        "corr-cap",
        move |id| {
            let runs = Arc::clone(&runs_clone);
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(format!("ok:{id}"))
            }
        },
    )
    .await;
    assert_eq!(
        report.outcome("unfit").unwrap().status,
        FanOutStatus::Unfit,
        "需求不满足的 worker 必须标记 Unfit：{:?}",
        report.outcomes
    );
    assert_eq!(
        report.outcome("fit").unwrap().status,
        FanOutStatus::Succeeded
    );
    assert_eq!(runs.load(Ordering::SeqCst), 1, "unfit worker 不得被调度");
}

#[tokio::test]
async fn fan_out_writes_experience_idempotently() {
    let workers: Vec<String> = vec!["w1".into(), "w2".into()];
    let store = ExperienceStore::in_memory();
    fan_out_cfg(
        &workers,
        FanOutConfig {
            max_parallel: 2,
            experience: Some(store.clone()),
            ..Default::default()
        },
        "corr-exp",
        |id| async move {
            if id == "w2" {
                Err("boom".into())
            } else {
                Ok("ok".into())
            }
        },
    )
    .await;
    assert_eq!(
        store.len(),
        2,
        "每个终态结果各一条（键 fan-out:corr:worker）"
    );
    let events = store.events();
    let w1 = events.iter().find(|e| e.worker == "w1").unwrap();
    assert_eq!(w1.outcome, Outcome::Success);
    assert_eq!(w1.kind, ExperienceKind::WorkerTask);
    let w2 = events.iter().find(|e| e.worker == "w2").unwrap();
    assert_eq!(w2.outcome, Outcome::Failure);
    assert_eq!(w2.attribution.error.as_deref(), Some("boom"));
}
#[tokio::test]
async fn bus_messages_carry_correlation_id_and_overflow_policy() {
    let bus = AgentBus::new();
    bus.register("worker-a", 4).await;
    bus.register("worker-b", 4).await;
    let delivered = bus
        .broadcast(
            "core",
            "topic",
            MessageKind::Progress,
            "corr-bcast",
            serde_json::json!({"n": 7}),
            OverflowPolicy::Reject,
        )
        .await;
    assert_eq!(delivered.len(), 2);
    for id in ["worker-a", "worker-b"] {
        let drained = bus.drain(id).await;
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].correlation_id, "corr-bcast");
        assert_eq!(drained[0].from, "core");
        assert_eq!(drained[0].payload, serde_json::json!({"n": 7}));
    }
    // 同一条消息（同 id）在两邮箱中保持字段一致。
    let a = bus.drain("worker-a").await;
    let b = bus.drain("worker-b").await;
    assert!(a.is_empty() && b.is_empty(), "drain 后邮箱为空");
}

fn sample_bus_message(id: u64, correlation_id: &str, payload: serde_json::Value) -> BusMessage {
    BusMessage {
        id,
        from: "core".to_string(),
        to: "w".to_string(),
        kind: MessageKind::Task,
        correlation_id: correlation_id.to_string(),
        payload,
    }
}

#[test]
fn dedupe_keys_distinguish_kind_and_payload() {
    let m1 = sample_bus_message(1, "c1", serde_json::json!({"a": 1}));
    let m2 = sample_bus_message(2, "c1", serde_json::json!({"a": 2}));
    assert_ne!(
        message_dedup_key(&m1),
        message_dedup_key(&m2),
        "载荷不同必须区分"
    );
    let m3 = BusMessage {
        kind: MessageKind::Progress,
        ..m1.clone()
    };
    assert_ne!(
        message_dedup_key(&m1),
        message_dedup_key(&m3),
        "消息种类不同必须区分"
    );
}

// ---------- worker 生命周期事件（总线） ----------

use owo_agent_core::fleet::{WorkerEvent, WorkerEventKind};

#[tokio::test]
async fn bus_send_worker_event_critical_under_overflow() {
    // worker 事件是关键语义：邮箱满时拒绝而非静默丢弃（DropMergeable 不适用）。
    let bus = AgentBus::new();
    bus.register("supervisor", 1).await;
    bus.send(
        "pool",
        "supervisor",
        MessageKind::Task,
        "corr-0",
        serde_json::json!({"busy": true}),
        OverflowPolicy::DropMergeable,
    )
    .await
    .unwrap();
    let event = WorkerEvent::new("w1", WorkerEventKind::Crashed, "boom", "corr-crash");
    let err = bus
        .send_worker_event("pool", "supervisor", &event)
        .await
        .unwrap_err();
    assert!(
        matches!(err, owo_agent_core::fleet::BusError::MailboxFull(_)),
        "关键事件满时必须以错误拒绝：{err}"
    );
    // 消费后恢复投递。
    let drained = bus.drain("supervisor").await;
    assert_eq!(drained.len(), 1);
}

#[tokio::test]
async fn bus_worker_event_payload_and_routing() {
    let bus = AgentBus::new();
    bus.register("supervisor", 16).await;
    let event = WorkerEvent::new(
        "w1",
        WorkerEventKind::Fused,
        "连续 3 次失败，熔断",
        "corr-fuse",
    );
    let id = bus
        .send_worker_event("pool", "supervisor", &event)
        .await
        .unwrap();
    let messages = bus.drain("supervisor").await;
    assert_eq!(messages.len(), 1);
    let msg = &messages[0];
    assert_eq!(msg.id, id);
    assert_eq!(msg.from, "pool");
    assert_eq!(msg.to, "supervisor");
    assert_eq!(msg.kind, MessageKind::Task);
    assert_eq!(msg.correlation_id, "corr-fuse");
    let parsed: WorkerEvent = serde_json::from_value(msg.payload.clone()).unwrap();
    assert_eq!(parsed.worker, "w1");
    assert_eq!(parsed.kind, WorkerEventKind::Fused);
    assert_eq!(parsed.detail, "连续 3 次失败，熔断");
    assert_eq!(parsed.correlation_id, "corr-fuse");
}

#[test]
fn worker_event_kind_serde_roundtrip() {
    for kind in [
        WorkerEventKind::Started,
        WorkerEventKind::Crashed,
        WorkerEventKind::Restarted,
        WorkerEventKind::Fused,
        WorkerEventKind::Stopped,
        WorkerEventKind::BudgetAborted,
        WorkerEventKind::Cancelled,
    ] {
        let json = serde_json::to_string(&kind).unwrap();
        let restored: WorkerEventKind = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, kind);
        assert!(!kind.label().is_empty());
    }
}

// ---------- R9：总线持久化与断点重放 ----------

#[tokio::test]
async fn bus_store_persist_and_replay_no_duplicate_execution() {
    let dir = std::env::temp_dir().join(format!("owo-bus-ft-{}", std::process::id()));
    let log = dir.join("bus.jsonl");
    let _ = std::fs::remove_dir_all(&dir);
    // 第一条总线：挂持久化存储，发送关键消息 + 重复提交 + 进度类消息。
    let store = BusStore::new(Some(log.clone())).unwrap();
    let bus1 = AgentBus::new();
    bus1.register("worker-a", 8).await;
    bus1.attach_store(store).await;
    bus1.send(
        "core",
        "worker-a",
        MessageKind::Task,
        "corr-1",
        serde_json::json!({"step": 1}),
        OverflowPolicy::Reject,
    )
    .await
    .unwrap();
    bus1.send(
        "core",
        "worker-a",
        MessageKind::Task,
        "corr-1",
        serde_json::json!({"step": 1}),
        OverflowPolicy::Reject,
    )
    .await
    .unwrap();
    bus1.send(
        "core",
        "worker-a",
        MessageKind::Progress,
        "corr-1",
        serde_json::json!({"p": 1}),
        OverflowPolicy::DropMergeable,
    )
    .await
    .unwrap();
    assert_eq!(
        bus1.store_len().await,
        1,
        "关键消息落盘且重复提交去重，进度类默认不落盘"
    );
    let drained1 = bus1.drain("worker-a").await;

    // 崩溃后：新总线挂同一存储重放，接收方幂等去重 → 不重复执行。
    let restored = BusStore::new(Some(log)).unwrap();
    let bus2 = AgentBus::new();
    bus2.register("worker-a", 8).await;
    bus2.attach_store(restored).await;
    let delivered = bus2.replay_store().await;
    assert_eq!(delivered, 1, "重放投递 1 条关键消息（进度类未落盘）");
    let drained2 = bus2.drain("worker-a").await;
    let merged = dedupe_messages(&[drained1.clone(), drained2].concat());
    let expected = dedupe_messages(&drained1).len();
    assert_eq!(
        merged.len(),
        expected,
        "重放不产生重复执行语义（幂等去重后消息集合不变）"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
