//! §12 支柱 1 .owflow 工作流引擎契约测试（≥20 项）。

use owo_agent_core::skill_health::SkillHealthStore;
use owo_agent_core::workflow::{
    eval_expr, validate_definition, ActSpec, AutoApprover, LocateSpec, MockBackend, PermMode,
    PermissionClaim, SenseSpec, TriggerKind, WorkflowDefinition, WorkflowEngine, WorkflowState,
    WorkflowStep, WorkflowTrigger,
};
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("owo-wf-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn trigger_manual() -> Vec<WorkflowTrigger> {
    vec![WorkflowTrigger {
        id: "trg-1".into(),
        kind: TriggerKind::Manual,
    }]
}

fn base_flow(id: &str) -> WorkflowDefinition {
    WorkflowDefinition {
        id: id.into(),
        name: format!("flow-{id}"),
        triggers: trigger_manual(),
        ..WorkflowDefinition::default()
    }
}

fn act_write(id: &str, path: &str, value: &str) -> WorkflowStep {
    WorkflowStep::Act {
        id: id.into(),
        scope: "fs.write".into(),
        spec: ActSpec {
            action: "write_file".into(),
            target: path.into(),
            value: Some(value.into()),
        },
    }
}

fn act_send(id: &str, target: &str, value: &str) -> WorkflowStep {
    WorkflowStep::Act {
        id: id.into(),
        scope: "message.send".into(),
        spec: ActSpec {
            action: "send_message".into(),
            target: target.into(),
            value: Some(value.into()),
        },
    }
}

/// happy-path 测试用：声明 fs.write 允许（默认 deny 由权限测试单独覆盖）。
fn fs_allow() -> Vec<PermissionClaim> {
    vec![PermissionClaim {
        scope: "fs.write".into(),
        mode: PermMode::Allow,
    }]
}

// ---------------------------------------------------------------------------
// 1. 序列化往返
// ---------------------------------------------------------------------------

#[test]
fn serde_round_trip_definition() {
    let flow = base_flow("serde");
    let flow = WorkflowDefinition {
        permissions: vec![PermissionClaim {
            scope: "fs.write".into(),
            mode: PermMode::Allow,
        }],
        steps: vec![
            act_write("s1", "a.txt", "hello"),
            WorkflowStep::Cond {
                id: "c1".into(),
                expr: "s1 != \"\"".into(),
                then: vec![act_write("s2", "b.txt", "x")],
                otherwise: vec![],
            },
        ],
        ..flow
    };
    let json = serde_json::to_string(&flow).unwrap();
    let back: WorkflowDefinition = serde_json::from_str(&json).unwrap();
    assert_eq!(back.id, flow.id);
    assert_eq!(back.permissions.len(), 1);
    assert_eq!(back.steps.len(), 2);
    assert_eq!(back.version, 1);
    // 再序列化一次仍然一致（无损）
    assert_eq!(serde_json::to_string(&back).unwrap(), json);
}

// ---------------------------------------------------------------------------
// 2-7. Schema 校验
// ---------------------------------------------------------------------------

#[test]
fn validate_rejects_empty_steps() {
    let flow = base_flow("v1");
    let errs = validate_definition(&flow, &[]).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("steps")));
}

#[test]
fn validate_rejects_duplicate_step_ids() {
    let mut flow = base_flow("v2");
    flow.steps = vec![
        act_write("dup", "a.txt", "1"),
        act_write("dup", "b.txt", "2"),
    ];
    let errs = validate_definition(&flow, &[]).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("重复")));
}

#[test]
fn validate_rejects_unknown_subflow_and_bad_rollback() {
    let mut flow = base_flow("v3");
    flow.steps = vec![WorkflowStep::Subflow {
        id: "sub".into(),
        flow: "nope".into(),
        args: BTreeMap::new(),
    }];
    let errs = validate_definition(&flow, &["real-flow".into()]).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("nope")));
    // 引用了存在的子流程则通过
    flow.steps = vec![WorkflowStep::Subflow {
        id: "sub".into(),
        flow: "real-flow".into(),
        args: BTreeMap::new(),
    }];
    assert!(validate_definition(&flow, &["real-flow".into()]).is_ok());

    // rollback_points 引用不存在步骤
    let mut flow2 = base_flow("v3b");
    flow2.rollback_points = vec!["missing".into()];
    flow2.steps = vec![act_write("s1", "a.txt", "1")];
    let errs = validate_definition(&flow2, &[]).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("missing")));
}

#[test]
fn validate_rejects_invalid_expr_and_missing_trigger() {
    let mut flow = base_flow("v4");
    flow.triggers = vec![];
    flow.steps = vec![WorkflowStep::Assert {
        id: "a1".into(),
        expr: "s1 ?? broken".into(),
        timeout_ms: 100,
    }];
    let errs = validate_definition(&flow, &[]).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("触发器")));
    assert!(errs.iter().any(|e| e.contains("表达式")));
}

#[test]
fn validate_rejects_zero_max_steps() {
    let mut flow = base_flow("v5");
    flow.max_steps = 0;
    flow.steps = vec![act_write("s1", "a.txt", "1")];
    let errs = validate_definition(&flow, &[]).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("max_steps")));
}

#[test]
fn compile_to_program_maps_control_flow() {
    use owo_agent_core::action_program::ProgramNode;
    let flow = base_flow("compile");
    let flow = WorkflowDefinition {
        steps: vec![
            act_write("s1", "a.txt", "x"),
            WorkflowStep::Cond {
                id: "c1".into(),
                expr: "true".into(),
                then: vec![act_write("s2", "b.txt", "y")],
                otherwise: vec![],
            },
            WorkflowStep::Loop {
                id: "l1".into(),
                body: vec![act_write("s3", "c.txt", "z")],
                cond: None,
                max_iter: 3,
            },
            WorkflowStep::Subflow {
                id: "sub1".into(),
                flow: "child".into(),
                args: BTreeMap::new(),
            },
        ],
        ..flow
    };
    let program = owo_agent_core::workflow::compile_to_program(&flow, &["child".into()]).unwrap();
    let kinds: Vec<&str> = program
        .nodes
        .iter()
        .map(|n| match n {
            ProgramNode::Step { .. } => "step",
            ProgramNode::Assert { .. } => "assert",
            ProgramNode::Branch { .. } => "branch",
            ProgramNode::Loop { .. } => "loop",
            ProgramNode::Sub { .. } => "sub",
            ProgramNode::WaitUntil { .. } => "wait",
            ProgramNode::Retry { .. } => "retry",
        })
        .collect();
    assert_eq!(kinds, vec!["step", "branch", "loop", "sub"]);
}

// ---------------------------------------------------------------------------
// 表达式求值
// ---------------------------------------------------------------------------

#[test]
fn eval_expr_cases() {
    let mut ctx = BTreeMap::new();
    ctx.insert("count".into(), json!(3));
    ctx.insert("name".into(), json!("ow"));
    assert!(eval_expr("count > 2", &ctx).unwrap());
    assert!(!eval_expr("count > 5", &ctx).unwrap());
    assert!(eval_expr("count == 3", &ctx).unwrap());
    assert!(eval_expr("name == \"ow\"", &ctx).unwrap());
    assert!(eval_expr("name != \"x\"", &ctx).unwrap());
    assert!(eval_expr("exists(count)", &ctx).unwrap());
    assert!(!eval_expr("exists(missing)", &ctx).unwrap());
    assert!(eval_expr("true", &ctx).unwrap());
    assert!(!eval_expr("false", &ctx).unwrap());
    assert!(eval_expr("count >= 3", &ctx).unwrap());
    assert!(eval_expr("count <= 3", &ctx).unwrap());
    assert!(eval_expr("count < 4", &ctx).unwrap());
    assert!(
        !eval_expr("unknown == 1", &ctx).unwrap(),
        "未知变量视为不成立"
    );
    assert!(eval_expr("garbage(", &ctx).is_err());
}

// ---------------------------------------------------------------------------
// 8-10. 线性执行 / 条件 / 循环
// ---------------------------------------------------------------------------

#[tokio::test]
async fn linear_execution_order_and_context() {
    let flow = base_flow("linear");
    let flow = WorkflowDefinition {
        permissions: fs_allow(),
        steps: vec![
            act_write("s1", "a.txt", "1"),
            act_write("s2", "b.txt", "2"),
            WorkflowStep::Assert {
                id: "a1".into(),
                expr: "exists(s1)".into(),
                timeout_ms: 100,
            },
            WorkflowStep::Notify {
                id: "n1".into(),
                message: "done".into(),
            },
        ],
        ..flow
    };
    let root = scratch("linear-ws");
    let mut engine = WorkflowEngine::new(
        flow.clone(),
        HashMap::new(),
        Box::new(MockBackend::new(root.clone())),
        Box::new(AutoApprover { approve: true }),
        SkillHealthStore::new(None),
        root.clone(),
    );
    let outcome = engine.run().await.unwrap();
    assert_eq!(outcome.state, WorkflowState::Succeeded);
    assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "1");
    assert_eq!(std::fs::read_to_string(root.join("b.txt")).unwrap(), "2");
    let kinds: Vec<&str> = outcome.steps.iter().map(|s| s.kind.as_str()).collect();
    assert_eq!(kinds, vec!["act", "act", "assert", "notify"]);
}

#[tokio::test]
async fn cond_branch_then_and_otherwise() {
    let flow = base_flow("cond");
    let flow = WorkflowDefinition {
        permissions: fs_allow(),
        steps: vec![
            act_write("s1", "a.txt", "v"),
            WorkflowStep::Cond {
                id: "c1".into(),
                expr: "exists(s1)".into(),
                then: vec![act_write("s2", "then.txt", "yes")],
                otherwise: vec![act_write("s3", "otherwise.txt", "no")],
            },
            WorkflowStep::Cond {
                id: "c2".into(),
                expr: "exists(not_here)".into(),
                then: vec![act_write("s4", "then2.txt", "yes")],
                otherwise: vec![act_write("s5", "otherwise2.txt", "no")],
            },
        ],
        ..flow
    };
    let root = scratch("cond-ws");
    let mut engine = WorkflowEngine::new(
        flow.clone(),
        HashMap::new(),
        Box::new(MockBackend::new(root.clone())),
        Box::new(AutoApprover { approve: true }),
        SkillHealthStore::new(None),
        root.clone(),
    );
    let outcome = engine.run().await.unwrap();
    assert_eq!(outcome.state, WorkflowState::Succeeded);
    assert!(root.join("then.txt").exists());
    assert!(!root.join("otherwise.txt").exists());
    assert!(!root.join("then2.txt").exists());
    assert!(root.join("otherwise2.txt").exists());
}

#[tokio::test]
async fn loop_iterations_and_early_exit_by_cond() {
    // cond 引用循环变量 {id}.iteration：iteration=3 时提前退出（max_iter=10 未用尽）。
    let flow = base_flow("loop");
    let flow = WorkflowDefinition {
        permissions: fs_allow(),
        steps: vec![
            act_write("s1", "count.txt", "0"),
            WorkflowStep::Loop {
                id: "l1".into(),
                body: vec![WorkflowStep::Act {
                    id: "inc".into(),
                    scope: "fs.write".into(),
                    spec: ActSpec {
                        action: "append_file".into(),
                        target: "count.txt".into(),
                        value: Some("+".into()),
                    },
                }],
                cond: Some("l1.iteration < 3".into()),
                max_iter: 10,
            },
        ],
        ..flow
    };
    let root = scratch("loop-ws");
    let mut engine = WorkflowEngine::new(
        flow.clone(),
        HashMap::new(),
        Box::new(MockBackend::new(root.clone())),
        Box::new(AutoApprover { approve: true }),
        SkillHealthStore::new(None),
        root.clone(),
    );
    let outcome = engine.run().await.unwrap();
    assert_eq!(outcome.state, WorkflowState::Succeeded);
    // iteration 0/1/2 为真共 3 次；3 为假提前退出（max_iter=10 未用尽）
    assert_eq!(
        std::fs::read_to_string(root.join("count.txt")).unwrap(),
        "0+++"
    );
}

#[tokio::test]
async fn loop_respects_max_iter() {
    let flow = base_flow("loop2");
    let flow = WorkflowDefinition {
        permissions: fs_allow(),
        steps: vec![
            act_write("s1", "count.txt", "0"),
            WorkflowStep::Loop {
                id: "l1".into(),
                body: vec![WorkflowStep::Act {
                    id: "inc".into(),
                    scope: "fs.write".into(),
                    spec: ActSpec {
                        action: "append_file".into(),
                        target: "count.txt".into(),
                        value: Some("+".into()),
                    },
                }],
                cond: Some("true".into()),
                max_iter: 3,
            },
        ],
        ..flow
    };
    let root = scratch("loop2-ws");
    let mut engine = WorkflowEngine::new(
        flow.clone(),
        HashMap::new(),
        Box::new(MockBackend::new(root.clone())),
        Box::new(AutoApprover { approve: true }),
        SkillHealthStore::new(None),
        root.clone(),
    );
    let outcome = engine.run().await.unwrap();
    assert_eq!(outcome.state, WorkflowState::Succeeded);
    assert_eq!(
        std::fs::read_to_string(root.join("count.txt")).unwrap(),
        "0+++"
    );
}

#[tokio::test]
async fn max_steps_breaker_stops_runaway_loop() {
    let flow = base_flow("runaway");
    let flow = WorkflowDefinition {
        max_steps: 20,
        permissions: fs_allow(),
        steps: vec![WorkflowStep::Loop {
            id: "l1".into(),
            body: vec![act_write("s1", "a.txt", "x")],
            cond: Some("true".into()),
            max_iter: 1000,
        }],
        ..flow
    };
    let root = scratch("runaway-ws");
    let mut engine = WorkflowEngine::new(
        flow.clone(),
        HashMap::new(),
        Box::new(MockBackend::new(root.clone())),
        Box::new(AutoApprover { approve: true }),
        SkillHealthStore::new(None),
        root.clone(),
    );
    let outcome = engine.run().await.unwrap();
    assert_eq!(outcome.state, WorkflowState::Failed);
    assert!(outcome.steps.len() < 30);
}

// ---------------------------------------------------------------------------
// 11-12. 子流程
// ---------------------------------------------------------------------------

#[tokio::test]
async fn subflow_recursion_with_args() {
    let child = base_flow("child");
    let child = WorkflowDefinition {
        id: "child".into(),
        name: "child".into(),
        triggers: trigger_manual(),
        steps: vec![act_write("w", "child.txt", "from-child")],
        ..child
    };
    let parent = base_flow("parent");
    let parent = WorkflowDefinition {
        permissions: fs_allow(),
        steps: vec![
            act_write("s1", "parent.txt", "p"),
            WorkflowStep::Subflow {
                id: "sub1".into(),
                flow: "child".into(),
                args: BTreeMap::new(),
            },
            WorkflowStep::Assert {
                id: "a1".into(),
                expr: "exists(sub1)".into(),
                timeout_ms: 100,
            },
        ],
        ..parent
    };
    let flows = HashMap::from([("child".to_string(), child)]);
    let root = scratch("sub-ws");
    let mut engine = WorkflowEngine::new(
        parent.clone(),
        flows,
        Box::new(MockBackend::new(root.clone())),
        Box::new(AutoApprover { approve: true }),
        SkillHealthStore::new(None),
        root.clone(),
    );
    let outcome = engine.run().await.unwrap();
    assert_eq!(outcome.state, WorkflowState::Succeeded);
    assert_eq!(
        std::fs::read_to_string(root.join("child.txt")).unwrap(),
        "from-child"
    );
}

#[tokio::test]
async fn subflow_failure_propagates() {
    let child = base_flow("child2");
    let child = WorkflowDefinition {
        id: "child2".into(),
        name: "child2".into(),
        triggers: trigger_manual(),
        steps: vec![WorkflowStep::Assert {
            id: "bad".into(),
            expr: "false".into(),
            timeout_ms: 100,
        }],
        ..child
    };
    let parent = base_flow("parent2");
    let parent = WorkflowDefinition {
        permissions: fs_allow(),
        steps: vec![WorkflowStep::Subflow {
            id: "sub1".into(),
            flow: "child2".into(),
            args: BTreeMap::new(),
        }],
        ..parent
    };
    let flows = HashMap::from([("child2".to_string(), child)]);
    let root = scratch("sub2-ws");
    let mut engine = WorkflowEngine::new(
        parent.clone(),
        flows,
        Box::new(MockBackend::new(root.clone())),
        Box::new(AutoApprover { approve: true }),
        SkillHealthStore::new(None),
        root.clone(),
    );
    let outcome = engine.run().await.unwrap();
    assert_eq!(outcome.state, WorkflowState::Failed);
}

// ---------------------------------------------------------------------------
// 13. 人审
// ---------------------------------------------------------------------------

#[tokio::test]
async fn human_approve_approved_continues() {
    let flow = base_flow("ha1");
    let flow = WorkflowDefinition {
        permissions: fs_allow(),
        steps: vec![
            WorkflowStep::HumanApprove {
                id: "h1".into(),
                prompt: "继续？".into(),
            },
            act_write("s1", "a.txt", "ok"),
        ],
        ..flow
    };
    let root = scratch("ha1-ws");
    let mut engine = WorkflowEngine::new(
        flow.clone(),
        HashMap::new(),
        Box::new(MockBackend::new(root.clone())),
        Box::new(AutoApprover { approve: true }),
        SkillHealthStore::new(None),
        root.clone(),
    );
    let outcome = engine.run().await.unwrap();
    assert_eq!(outcome.state, WorkflowState::Succeeded);
    assert!(root.join("a.txt").exists());
}

#[tokio::test]
async fn human_approve_rejected_aborts_workflow() {
    let flow = base_flow("ha2");
    let flow = WorkflowDefinition {
        steps: vec![
            WorkflowStep::HumanApprove {
                id: "h1".into(),
                prompt: "继续？".into(),
            },
            act_write("s1", "a.txt", "should-not-write"),
        ],
        ..flow
    };
    let root = scratch("ha2-ws");
    let mut engine = WorkflowEngine::new(
        flow.clone(),
        HashMap::new(),
        Box::new(MockBackend::new(root.clone())),
        Box::new(AutoApprover { approve: false }),
        SkillHealthStore::new(None),
        root.clone(),
    );
    let outcome = engine.run().await.unwrap();
    assert_eq!(outcome.state, WorkflowState::Failed);
    assert!(!root.join("a.txt").exists(), "拒绝后后续步骤不得执行");
    let kinds: Vec<&str> = outcome.steps.iter().map(|s| s.kind.as_str()).collect();
    assert_eq!(kinds, vec!["human_approve"]);
}

// ---------------------------------------------------------------------------
// 14-16. 权限
// ---------------------------------------------------------------------------

#[tokio::test]
async fn permission_default_deny_blocks_act() {
    let flow = base_flow("perm1");
    // 未声明任何权限 → 默认 deny
    let flow = WorkflowDefinition {
        steps: vec![act_send("s1", "chat", "hello")],
        ..flow
    };
    let root = scratch("perm1-ws");
    let mut engine = WorkflowEngine::new(
        flow.clone(),
        HashMap::new(),
        Box::new(MockBackend::new(root.clone())),
        Box::new(AutoApprover { approve: true }),
        SkillHealthStore::new(None),
        root.clone(),
    );
    let outcome = engine.run().await.unwrap();
    assert_eq!(outcome.state, WorkflowState::Failed);
    assert!(
        !root.join("messages.log").exists(),
        "默认 deny 不得执行动作"
    );
    let denied = engine
        .audit()
        .entries
        .iter()
        .any(|e| e.event == "workflow.permission_deny");
    assert!(denied, "权限拒绝必须审计");
}

#[tokio::test]
async fn permission_allow_passes() {
    let flow = base_flow("perm2");
    let flow = WorkflowDefinition {
        permissions: vec![PermissionClaim {
            scope: "message.send".into(),
            mode: PermMode::Allow,
        }],
        steps: vec![act_send("s1", "chat", "hello")],
        ..flow
    };
    let root = scratch("perm2-ws");
    let mut engine = WorkflowEngine::new(
        flow.clone(),
        HashMap::new(),
        Box::new(MockBackend::new(root.clone())),
        Box::new(AutoApprover { approve: true }),
        SkillHealthStore::new(None),
        root.clone(),
    );
    let outcome = engine.run().await.unwrap();
    assert_eq!(outcome.state, WorkflowState::Succeeded);
    let log = std::fs::read_to_string(root.join("messages.log")).unwrap();
    assert!(log.contains("hello"));
}

#[tokio::test]
async fn permission_ask_respects_approver() {
    let flow = base_flow("perm3");
    let flow = WorkflowDefinition {
        permissions: vec![PermissionClaim {
            scope: "fs.write".into(),
            mode: PermMode::Ask,
        }],
        steps: vec![act_write("s1", "a.txt", "x")],
        ..flow
    };
    let root = scratch("perm3-ws");
    let mut engine = WorkflowEngine::new(
        flow.clone(),
        HashMap::new(),
        Box::new(MockBackend::new(root.clone())),
        Box::new(AutoApprover { approve: false }),
        SkillHealthStore::new(None),
        root.clone(),
    );
    let outcome = engine.run().await.unwrap();
    assert_eq!(outcome.state, WorkflowState::Failed);
    assert!(!root.join("a.txt").exists());
}

// ---------------------------------------------------------------------------
// 17-18. 健康度
// ---------------------------------------------------------------------------

#[tokio::test]
async fn skill_health_disabled_rejected() {
    let flow = base_flow("health1");
    let flow = WorkflowDefinition {
        steps: vec![WorkflowStep::InvokeSkill {
            id: "sk1".into(),
            skill: "bad-skill".into(),
            args: BTreeMap::new(),
        }],
        ..flow
    };
    let root = scratch("health1-ws");
    let mut engine = WorkflowEngine::new(
        flow.clone(),
        HashMap::new(),
        Box::new(MockBackend::new(root.clone())),
        Box::new(AutoApprover { approve: true }),
        SkillHealthStore::new(None),
        root.clone(),
    );
    engine.disable_skill("bad-skill");
    let outcome = engine.run().await.unwrap();
    assert_eq!(outcome.state, WorkflowState::Failed);
    assert!(
        outcome
            .steps
            .iter()
            .any(|s| !s.ok && s.kind == "invoke_skill"),
        "禁用技能必须拒绝并记录失败"
    );
}

#[tokio::test]
async fn skill_health_degraded_requires_confirm_and_records_outcome() {
    // 造 Degraded：连续两次失败
    let mut health = SkillHealthStore::new(None);
    for _ in 0..2 {
        health.record("flaky", false, None).unwrap();
    }
    assert_eq!(
        owo_agent_core::skill_health::SkillState::Degraded,
        health.state("flaky")
    );

    // Degraded + 拒绝确认 → 不执行
    let flow = base_flow("health2");
    let flow = WorkflowDefinition {
        steps: vec![WorkflowStep::InvokeSkill {
            id: "sk1".into(),
            skill: "flaky".into(),
            args: BTreeMap::new(),
        }],
        ..flow
    };
    let root = scratch("health2-ws");
    let mut engine = WorkflowEngine::new(
        flow.clone(),
        HashMap::new(),
        Box::new(MockBackend::new(root.clone())),
        Box::new(AutoApprover { approve: false }),
        health,
        root.clone(),
    );
    let outcome = engine.run().await.unwrap();
    assert_eq!(outcome.state, WorkflowState::Failed);

    // Degraded + 确认 → 执行；成功后回写 health
    let mut health = SkillHealthStore::new(None);
    for _ in 0..2 {
        health.record("flaky2", false, None).unwrap();
    }
    let flow2 = base_flow("health3");
    let flow2 = WorkflowDefinition {
        steps: vec![WorkflowStep::InvokeSkill {
            id: "sk1".into(),
            skill: "flaky2".into(),
            args: BTreeMap::new(),
        }],
        ..flow2
    };
    let root2 = scratch("health3-ws");
    let mut engine2 = WorkflowEngine::new(
        flow2.clone(),
        HashMap::new(),
        Box::new(MockBackend::new(root2.clone())),
        Box::new(AutoApprover { approve: true }),
        health,
        root2.clone(),
    );
    let outcome2 = engine2.run().await.unwrap();
    assert_eq!(outcome2.state, WorkflowState::Succeeded);
}

// ---------------------------------------------------------------------------
// 19-21. 回滚
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rollback_on_failure_restores_work_tree() {
    let flow = base_flow("rollback1");
    let flow = WorkflowDefinition {
        permissions: fs_allow(),
        steps: vec![
            act_write("s1", "keep.txt", "before"),
            WorkflowStep::RollbackPoint {
                id: "cp1".to_string(),
            },
            act_write("s2", "later.txt", "after-checkpoint"),
            WorkflowStep::Assert {
                id: "a1".into(),
                expr: "false".into(),
                timeout_ms: 100,
            },
        ],
        ..flow
    };
    let root = scratch("rollback1-ws");
    std::fs::write(root.join("keep.txt"), "before").unwrap();
    let mut engine = WorkflowEngine::new(
        flow.clone(),
        HashMap::new(),
        Box::new(MockBackend::new(root.clone())),
        Box::new(AutoApprover { approve: true }),
        SkillHealthStore::new(None),
        root.clone(),
    );
    let outcome = engine.run().await.unwrap();
    assert_eq!(outcome.state, WorkflowState::Failed);
    assert_eq!(
        outcome.rollback_to.as_deref(),
        Some("cp1"),
        "应回滚到最近检查点 cp1"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("keep.txt")).unwrap(),
        "before"
    );
    assert!(
        !root.join("later.txt").exists(),
        "检查点之后写入的文件应被回滚删除"
    );
}

#[tokio::test]
async fn rollback_restores_modified_file_content() {
    let flow = base_flow("rollback2");
    let flow = WorkflowDefinition {
        permissions: fs_allow(),
        steps: vec![
            act_write("s1", "doc.txt", "version-1"),
            WorkflowStep::RollbackPoint {
                id: "cp1".to_string(),
            },
            act_write("s2", "doc.txt", "version-2"),
            WorkflowStep::Assert {
                id: "a1".into(),
                expr: "false".into(),
                timeout_ms: 100,
            },
        ],
        ..flow
    };
    let root = scratch("rollback2-ws");
    let mut engine = WorkflowEngine::new(
        flow.clone(),
        HashMap::new(),
        Box::new(MockBackend::new(root.clone())),
        Box::new(AutoApprover { approve: true }),
        SkillHealthStore::new(None),
        root.clone(),
    );
    let outcome = engine.run().await.unwrap();
    assert_eq!(outcome.state, WorkflowState::Failed);
    assert_eq!(
        std::fs::read_to_string(root.join("doc.txt")).unwrap(),
        "version-1",
        "回滚后文件应恢复检查点内容"
    );
}

#[tokio::test]
async fn rollback_to_nearest_checkpoint() {
    let flow = base_flow("rollback3");
    let flow = WorkflowDefinition {
        permissions: fs_allow(),
        steps: vec![
            act_write("s1", "a.txt", "1"),
            WorkflowStep::RollbackPoint {
                id: "cp1".to_string(),
            },
            act_write("s2", "b.txt", "2"),
            WorkflowStep::RollbackPoint {
                id: "cp2".to_string(),
            },
            act_write("s3", "c.txt", "3"),
            WorkflowStep::Assert {
                id: "a1".into(),
                expr: "false".into(),
                timeout_ms: 100,
            },
        ],
        ..flow
    };
    let root = scratch("rollback3-ws");
    let mut engine = WorkflowEngine::new(
        flow.clone(),
        HashMap::new(),
        Box::new(MockBackend::new(root.clone())),
        Box::new(AutoApprover { approve: true }),
        SkillHealthStore::new(None),
        root.clone(),
    );
    let outcome = engine.run().await.unwrap();
    assert_eq!(outcome.state, WorkflowState::Failed);
    assert_eq!(outcome.rollback_to.as_deref(), Some("cp2"));
    // cp2 之后写入的 c.txt 被回滚；cp1 之后的 b.txt 保留
    assert!(!root.join("c.txt").exists());
    assert!(root.join("b.txt").exists());
    assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "1");
}

// ---------------------------------------------------------------------------
// 22-24. 前置条件 / abort / 审计
// ---------------------------------------------------------------------------

#[tokio::test]
async fn precondition_fail_does_not_start() {
    let flow = base_flow("pre");
    let flow = WorkflowDefinition {
        preconditions: vec!["ready == true".into()],
        steps: vec![act_write("s1", "a.txt", "x")],
        ..flow
    };
    let root = scratch("pre-ws");
    let mut engine = WorkflowEngine::new(
        flow.clone(),
        HashMap::new(),
        Box::new(MockBackend::new(root.clone())),
        Box::new(AutoApprover { approve: true }),
        SkillHealthStore::new(None),
        root.clone(),
    );
    let outcome = engine.run().await.unwrap();
    assert_eq!(outcome.state, WorkflowState::Failed);
    assert!(outcome.steps.is_empty());
    assert!(!root.join("a.txt").exists());
}

#[tokio::test]
async fn abort_before_run_stops_immediately() {
    let flow = base_flow("abort");
    let flow = WorkflowDefinition {
        steps: vec![act_write("s1", "a.txt", "x")],
        ..flow
    };
    let root = scratch("abort-ws");
    let mut engine = WorkflowEngine::new(
        flow.clone(),
        HashMap::new(),
        Box::new(MockBackend::new(root.clone())),
        Box::new(AutoApprover { approve: true }),
        SkillHealthStore::new(None),
        root.clone(),
    );
    engine.abort();
    let outcome = engine.run().await.unwrap();
    assert_eq!(outcome.state, WorkflowState::Aborted);
    assert!(outcome.steps.is_empty());
    assert!(!root.join("a.txt").exists());
}

#[tokio::test]
async fn audit_tracks_start_success_and_steps() {
    let flow = base_flow("audit");
    let flow = WorkflowDefinition {
        permissions: vec![PermissionClaim {
            scope: "fs.write".into(),
            mode: PermMode::Allow,
        }],
        steps: vec![act_write("s1", "a.txt", "x")],
        ..flow
    };
    let root = scratch("audit-ws");
    let mut engine = WorkflowEngine::new(
        flow.clone(),
        HashMap::new(),
        Box::new(MockBackend::new(root.clone())),
        Box::new(AutoApprover { approve: true }),
        SkillHealthStore::new(None),
        root.clone(),
    );
    engine.run().await.unwrap();
    let events: Vec<&str> = engine
        .audit()
        .entries
        .iter()
        .map(|e| e.event.as_str())
        .collect();
    assert!(events.contains(&"workflow.start"));
    assert!(events.contains(&"workflow.succeeded"));
}

// ---------------------------------------------------------------------------
// 25. 样例：整理表格 → 生成文档 → 人审
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sample_organize_table_generate_doc_human_review() {
    let flow = base_flow("sample");
    let flow = WorkflowDefinition {
        name: "整理表格→生成文档→人审".into(),
        permissions: vec![
            PermissionClaim {
                scope: "fs.write".into(),
                mode: PermMode::Allow,
            },
            PermissionClaim {
                scope: "message.send".into(),
                mode: PermMode::Ask,
            },
        ],
        steps: vec![
            WorkflowStep::Sense {
                id: "tables".into(),
                spec: SenseSpec {
                    target: "spreadsheet".into(),
                },
            },
            WorkflowStep::Locate {
                id: "row".into(),
                spec: LocateSpec {
                    target: "data-rows".into(),
                },
            },
            act_write("doc", "report.md", "# 汇总报告"),
            WorkflowStep::Cond {
                id: "has_data".into(),
                expr: "exists(row)".into(),
                then: vec![WorkflowStep::Act {
                    id: "append".into(),
                    scope: "fs.write".into(),
                    spec: ActSpec {
                        action: "append_file".into(),
                        target: "report.md".into(),
                        value: Some("\n- 数据行已并入".into()),
                    },
                }],
                otherwise: vec![],
            },
            WorkflowStep::HumanApprove {
                id: "review".into(),
                prompt: "报告已生成，是否发送？".into(),
            },
            act_send("send", "reviewer", "报告完成"),
        ],
        ..flow
    };
    let root = scratch("sample-ws");
    let mut backend = MockBackend::new(root.clone());
    backend
        .sense_results
        .insert("spreadsheet".into(), json!({ "sheets": ["2026"] }));
    backend
        .locate_results
        .insert("data-rows".into(), json!([1, 2, 3]));
    let mut engine = WorkflowEngine::new(
        flow.clone(),
        HashMap::new(),
        Box::new(backend),
        Box::new(AutoApprover { approve: true }),
        SkillHealthStore::new(None),
        root.clone(),
    );
    let outcome = engine.run().await.unwrap();
    assert_eq!(outcome.state, WorkflowState::Succeeded);
    let report = std::fs::read_to_string(root.join("report.md")).unwrap();
    assert!(report.contains("# 汇总报告"));
    assert!(report.contains("数据行已并入"));
    let log = std::fs::read_to_string(root.join("messages.log")).unwrap();
    assert!(log.contains("报告完成"));
}

// ---------------------------------------------------------------------------
// 26. 权限声明序列化
// ---------------------------------------------------------------------------

#[test]
fn permission_modes_serialize_snake_case() {
    let claim = PermissionClaim {
        scope: "fs.write".into(),
        mode: PermMode::Ask,
    };
    let json = serde_json::to_string(&claim).unwrap();
    assert!(json.contains("\"ask\""));
    let back: PermissionClaim = serde_json::from_str(&json).unwrap();
    assert_eq!(back.mode, PermMode::Ask);
}
