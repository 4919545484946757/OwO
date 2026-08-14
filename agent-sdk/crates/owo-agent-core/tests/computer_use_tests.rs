//! M4d computer-use 审批版闭环契约测试。
//!
//! 覆盖：动作门禁（未批准/应用不匹配/超时/预算/敏感熔断）、resume 恢复、
//! 感知闭环（Mock 面全流程）、每步审计。全部无网络/无真实桌面依赖。

use owo_agent_core::audit::AuditLog;
use owo_agent_core::computer_task::{
    sensitive_ui_hit, ComputerTask, ComputerTaskRegistry, TaskState,
};
use owo_agent_core::computer_use::{
    run_approved_task_on, scan_ui_sensitive, task_gate_check, TaskGoal, TaskSurface,
};
use serde_json::{json, Value};
use std::time::Duration;

fn sample_task(id: &str) -> ComputerTask {
    ComputerTask {
        id: id.to_string(),
        target_app: "owo-sim-qq".to_string(),
        description: "模拟 QQ 受控发送".to_string(),
        allowed_actions: vec![
            "desktop_click".to_string(),
            "desktop_type".to_string(),
            "desktop_key".to_string(),
        ],
        max_duration_ms: 60_000,
        state: TaskState::Pending,
        created_at: "2026-08-14T00:00:00Z".to_string(),
        fuse_reason: None,
    }
}

fn line(text: &str, x: i32, y: i32, role: &str) -> Value {
    json!({
        "text": text,
        "x": x,
        "y": y,
        "width": 200,
        "height": 30,
        "role_hint": role,
    })
}

/// 内存模拟桌面面：聊天窗口版面 + 输入框/发送按钮/消息上屏。
struct MockTaskSurface {
    app: String,
    lines: Vec<Value>,
    input: String,
    sent: Vec<String>,
    clicks: Vec<(i32, i32)>,
    typed: Vec<String>,
    keys: Vec<String>,
    /// 输入包含该关键词时下一帧弹出敏感对话框（密码框）。
    sensitive_after_typing: Option<String>,
}

impl MockTaskSurface {
    fn chat() -> Self {
        Self {
            app: "owo-sim-qq".to_string(),
            lines: vec![
                line("会话：张子豪", 10, 10, "header"),
                line("输入消息...", 10, 500, "input"),
                line("发送", 600, 500, "button"),
            ],
            input: String::new(),
            sent: Vec::new(),
            clicks: Vec::new(),
            typed: Vec::new(),
            keys: Vec::new(),
            sensitive_after_typing: None,
        }
    }

    fn sent_count(&self) -> usize {
        self.sent.len()
    }
}

#[async_trait::async_trait]
impl TaskSurface for MockTaskSurface {
    fn app(&self) -> String {
        self.app.clone()
    }

    async fn ocr(&mut self) -> Result<Value, String> {
        Ok(json!({ "lines": self.lines, "surface": "mock" }))
    }

    async fn click(&mut self, x: i32, y: i32) -> Result<(), String> {
        self.clicks.push((x, y));
        // 点击输入框行 → 聚焦（无副作用即可）。
        Ok(())
    }

    async fn type_text(&mut self, text: &str) -> Result<(), String> {
        self.typed.push(text.to_string());
        self.input.push_str(text);
        if let Some(keyword) = &self.sensitive_after_typing {
            if text.contains(keyword) {
                self.lines.push(line("请输入支付密码", 300, 300, "edit"));
            }
        }
        Ok(())
    }

    async fn key(&mut self, key: &str) -> Result<(), String> {
        self.keys.push(key.to_string());
        if key == "enter" {
            let message = self.input.clone();
            self.input.clear();
            if !message.is_empty() {
                self.sent.push(message.clone());
                // 消息上屏，供 verify_text 命中。
                self.lines
                    .push(line(&format!("我：{message}"), 10, 80, "message"));
            }
        }
        Ok(())
    }

    async fn launch(&mut self, _target: &str) -> Result<(), String> {
        Ok(())
    }
}

fn closed_loop_goals(value: &str) -> Vec<TaskGoal> {
    vec![
        TaskGoal {
            anchor_text: "输入消息".to_string(),
            action: "click".to_string(),
            value: String::new(),
            verify_text: None,
        },
        TaskGoal {
            anchor_text: "输入消息".to_string(),
            action: "type".to_string(),
            value: value.to_string(),
            verify_text: None,
        },
        TaskGoal {
            anchor_text: "发送".to_string(),
            action: "click".to_string(),
            value: String::new(),
            verify_text: None,
        },
        TaskGoal {
            anchor_text: "发送".to_string(),
            action: "key".to_string(),
            value: "enter".to_string(),
            verify_text: Some(value.to_string()),
        },
    ]
}

fn approved_running(registry: &ComputerTaskRegistry, id: &str) {
    registry.approve(id).unwrap();
    registry.start(id).unwrap();
}

#[tokio::test]
async fn unapproved_action_denied_before_any_execution() {
    let registry = ComputerTaskRegistry::new();
    registry.create(sample_task("t1")).unwrap();
    let mut audit = AuditLog::default();
    let mut surface = MockTaskSurface::chat();

    let error = run_approved_task_on(
        &registry,
        &mut audit,
        "s",
        "t1",
        &closed_loop_goals("未批准-001"),
        &mut surface,
    )
    .await
    .unwrap_err();
    assert!(error.contains("需用户先批准"));
    assert_eq!(surface.sent_count(), 0);
    assert!(surface.clicks.is_empty());
    assert!(surface.typed.is_empty());
    assert!(registry.get("t1").unwrap().state == TaskState::Pending);
}

#[tokio::test]
async fn approved_task_runs_full_closed_loop_on_mock_surface() {
    let registry = ComputerTaskRegistry::new();
    registry.create(sample_task("t2")).unwrap();
    approved_running(&registry, "t2");
    let mut audit = AuditLog::default();
    let mut surface = MockTaskSurface::chat();

    let report = run_approved_task_on(
        &registry,
        &mut audit,
        "s",
        "t2",
        &closed_loop_goals("审批版闭环-001"),
        &mut surface,
    )
    .await
    .unwrap();
    assert_eq!(report.steps, 4);
    assert_eq!(report.state, TaskState::Completed);
    assert_eq!(surface.sent_count(), 1);
    assert_eq!(surface.sent[0], "审批版闭环-001");
    assert_eq!(registry.get("t2").unwrap().state, TaskState::Completed);
    // 动作预算计数=4。
    assert_eq!(registry.actions_taken("t2"), 4);
}

#[tokio::test]
async fn target_app_mismatch_denied_before_execution() {
    let registry = ComputerTaskRegistry::new();
    registry.create(sample_task("t3")).unwrap();
    approved_running(&registry, "t3");
    let mut audit = AuditLog::default();
    let mut surface = MockTaskSurface::chat();
    surface.app = "qq".to_string();

    let error = run_approved_task_on(
        &registry,
        &mut audit,
        "s",
        "t3",
        &closed_loop_goals("越界-001"),
        &mut surface,
    )
    .await
    .unwrap_err();
    assert!(error.contains("目标应用"));
    assert!(surface.clicks.is_empty());
    assert!(surface.typed.is_empty());
}

#[tokio::test]
async fn sensitive_ui_fuses_and_blocks_until_resume() {
    let registry = ComputerTaskRegistry::new();
    registry.create(sample_task("t4")).unwrap();
    approved_running(&registry, "t4");
    let mut audit = AuditLog::default();
    let mut surface = MockTaskSurface::chat();
    surface.sensitive_after_typing = Some("密码".to_string());

    let goals = vec![
        TaskGoal {
            anchor_text: "输入消息".to_string(),
            action: "click".to_string(),
            value: String::new(),
            verify_text: None,
        },
        TaskGoal {
            anchor_text: "输入消息".to_string(),
            action: "type".to_string(),
            value: "我的支付密码是xxx".to_string(),
            verify_text: None,
        },
        TaskGoal {
            anchor_text: "发送".to_string(),
            action: "click".to_string(),
            value: String::new(),
            verify_text: None,
        },
    ];
    let error = run_approved_task_on(&registry, &mut audit, "s", "t4", &goals, &mut surface)
        .await
        .unwrap_err();
    assert!(error.contains("敏感熔断"));
    assert_eq!(registry.get("t4").unwrap().state, TaskState::Fused);
    assert!(registry.get("t4").unwrap().fuse_reason.is_some());
    // 熔断后：下一步动作未执行（第 3 步 click 未发生；只有前两步动作）。
    assert_eq!(surface.clicks.len(), 1);
    assert_eq!(surface.typed.len(), 1);

    // 熔断后所有动作被拒，直到人工接管 resume。
    let denied = task_gate_check(
        &registry,
        Some(&mut audit),
        "s",
        "t4",
        "desktop_click",
        "owo-sim-qq",
        None,
    );
    assert!(denied.is_err());
    assert!(denied.unwrap_err().contains("不可执行"));

    // 人工接管（resume）后可继续执行。
    assert_eq!(registry.resume("t4").unwrap(), TaskState::Approved);
    assert!(registry.check_can_execute("t4").is_ok());
    // 敏感界面已消失后，重新跑闭环可以完成。
    surface
        .lines
        .retain(|l| l.get("text").and_then(Value::as_str) != Some("请输入支付密码"));
    surface.sensitive_after_typing = None;
    let report = run_approved_task_on(
        &registry,
        &mut audit,
        "s",
        "t4",
        &[
            TaskGoal {
                anchor_text: "输入消息".to_string(),
                action: "type".to_string(),
                value: "恢复后继续-002".to_string(),
                verify_text: None,
            },
            TaskGoal {
                anchor_text: "发送".to_string(),
                action: "key".to_string(),
                value: "enter".to_string(),
                verify_text: Some("恢复后继续-002".to_string()),
            },
        ],
        &mut surface,
    )
    .await
    .unwrap();
    assert_eq!(report.state, TaskState::Completed);
    assert!(surface.sent.iter().any(|s| s.contains("恢复后继续-002")));
}

#[tokio::test]
async fn timeout_pauses_loop_and_denies() {
    let registry = ComputerTaskRegistry::new();
    let mut task = sample_task("t5");
    task.max_duration_ms = 10;
    registry.create(task).unwrap();
    approved_running(&registry, "t5");
    std::thread::sleep(Duration::from_millis(20));
    let mut audit = AuditLog::default();
    let mut surface = MockTaskSurface::chat();

    let error = run_approved_task_on(
        &registry,
        &mut audit,
        "s",
        "t5",
        &closed_loop_goals("超时-001"),
        &mut surface,
    )
    .await
    .unwrap_err();
    assert!(error.contains("超时"));
    assert_eq!(registry.get("t5").unwrap().state, TaskState::Paused);
    assert!(surface.clicks.is_empty());
}

#[tokio::test]
async fn action_budget_exhausted_pauses_and_denies() {
    let registry = ComputerTaskRegistry::new();
    registry.create(sample_task("t6")).unwrap();
    registry.set_action_budget("t6", 2).unwrap();
    approved_running(&registry, "t6");
    let mut audit = AuditLog::default();
    let mut surface = MockTaskSurface::chat();

    // 预算 2：前两步（click/type）放行，第三步 click 应被拒并自动暂停。
    let error = run_approved_task_on(
        &registry,
        &mut audit,
        "s",
        "t6",
        &closed_loop_goals("预算-001"),
        &mut surface,
    )
    .await
    .unwrap_err();
    assert!(error.contains("预算"));
    assert_eq!(registry.get("t6").unwrap().state, TaskState::Paused);
    assert_eq!(surface.clicks.len(), 1); // 只有第一步 click 执行
    assert_eq!(surface.typed.len(), 1);
    assert_eq!(surface.keys.len(), 0);
    assert_eq!(registry.actions_taken("t6"), 2);
}

#[tokio::test]
async fn audit_records_denied_and_allowed_entries() {
    let registry = ComputerTaskRegistry::new();
    registry.create(sample_task("t7")).unwrap();
    let mut audit = AuditLog::default();

    // 未批准直接调用门禁 → 拒绝条目（approved=false）。
    let err = task_gate_check(
        &registry,
        Some(&mut audit),
        "s",
        "t7",
        "desktop_click",
        "owo-sim-qq",
        None,
    )
    .unwrap_err();
    assert!(err.contains("Pending"));

    approved_running(&registry, "t7");
    task_gate_check(
        &registry,
        Some(&mut audit),
        "s",
        "t7",
        "desktop_click",
        "owo-sim-qq",
        None,
    )
    .unwrap();
    registry.record_action("t7");

    let entries = audit.entries.clone();
    let denied = entries.iter().find(|e| e.approved == Some(false));
    assert!(denied.is_some(), "应有拒绝审计条目");
    assert!(denied.unwrap().detail.contains("门禁拒绝"));
    let allowed = entries.iter().find(|e| e.approved == Some(true));
    assert!(allowed.is_some(), "应有放行审计条目");
}

#[tokio::test]
async fn closed_loop_audits_start_step_and_complete() {
    let registry = ComputerTaskRegistry::new();
    registry.create(sample_task("t8")).unwrap();
    approved_running(&registry, "t8");
    let mut audit = AuditLog::default();
    let mut surface = MockTaskSurface::chat();

    let _ = run_approved_task_on(
        &registry,
        &mut audit,
        "s",
        "t8",
        &closed_loop_goals("审计-001"),
        &mut surface,
    )
    .await
    .unwrap();

    let events: Vec<String> = audit.entries.iter().map(|e| e.event.clone()).collect();
    assert!(events.iter().any(|e| e == "computer_task"));
    let tools: Vec<Option<String>> = audit.entries.iter().map(|e| e.tool.clone()).collect();
    assert!(tools.iter().any(|t| t.as_deref() == Some("start")));
    assert!(tools.iter().any(|t| t.as_deref() == Some("step")));
    assert!(tools.iter().any(|t| t.as_deref() == Some("complete")));
    // 门禁放行条目（permission/approved=true）。
    assert!(audit
        .entries
        .iter()
        .any(|e| e.event == "permission" && e.approved == Some(true)));
}

#[test]
fn gate_and_sensitive_pure_function_semantics() {
    // sensitive_ui_hit 关键词覆盖（密码/支付/验证码）。
    assert!(sensitive_ui_hit("PasswordBox", "Edit", "").is_some());
    assert!(sensitive_ui_hit("", "", "请输入验证码").is_some());
    assert!(sensitive_ui_hit("输入消息", "Edit", "").is_none());
    // scan_ui_sensitive 对整屏 lines 扫描。
    let clean =
        json!({ "lines": [line("输入消息...", 0, 0, "input"), line("发送", 0, 0, "button")] });
    assert!(scan_ui_sensitive(&clean).is_none());
    let dirty = json!({ "lines": [line("请输入支付密码", 0, 0, "edit")] });
    assert!(scan_ui_sensitive(&dirty).is_some());
}

#[tokio::test]
async fn sim_surface_requires_env_or_returns_clear_error() {
    // 无 OWO_SIM_QQ_URL 时 SimTaskSurface::new 返回明确错误（含配置提示）。
    if std::env::var("OWO_SIM_QQ_URL").is_err() {
        let error = owo_agent_core::computer_use::SimTaskSurface::new().unwrap_err();
        assert!(error.contains("OWO_SIM_QQ_URL"));
    }
}

#[tokio::test]
async fn run_approved_task_on_live_sim_when_configured() {
    // 可选：OWO_SIM_QQ_URL 指向 owo-sim-qq 时跑真实模拟面闭环（与 e2e 脚本同路径）。
    let Ok(base) = std::env::var("OWO_SIM_QQ_URL") else {
        eprintln!("[skip] 未配置 OWO_SIM_QQ_URL，跳过 live-sim 闭环测试");
        return;
    };
    let client = reqwest::Client::new();
    let _ = client
        .post(format!("{}/reset", base.trim_end_matches('/')))
        .json(&json!({}))
        .send()
        .await
        .expect("模拟服务不可达");

    let registry = ComputerTaskRegistry::new();
    registry.create(sample_task("live1")).unwrap();
    registry.approve("live1").unwrap();
    registry.start("live1").unwrap();
    let mut audit = AuditLog::default();
    let mut surface = owo_agent_core::computer_use::SimTaskSurface::new().unwrap();

    let report = run_approved_task_on(
        &registry,
        &mut audit,
        "s",
        "live1",
        &closed_loop_goals("模拟面闭环-001"),
        &mut surface,
    )
    .await
    .unwrap();
    assert_eq!(report.state, TaskState::Completed);
    assert_eq!(report.steps, 4);
    assert_eq!(registry.get("live1").unwrap().state, TaskState::Completed);
    eprintln!("[live-sim] 闭环完成，4 步全过");
}
