//! computer-use 任务级审批（m4d 前奏，v1）。
//!
//! 把"任意 desktop_* 操作"收敛为**任务上下文**：执行 computer-use 操作前必须先创建
//! 并批准一个任务（目标应用 + 任务描述 + 时长上限 + 允许动作集），批准后在该任务内
//! 执行；检测到敏感 UI（密码/支付/验证码）立即熔断暂停并要求人工接管。任务全生命周期
//! 写审计日志（创建/批准/执行/熔断/完成）。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 默认动作次数预算（每个任务，超出后自动暂停并要求人工接管）。
pub const DEFAULT_ACTION_BUDGET: u32 = 200;

/// computer-use 任务状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    /// 已创建，等待用户批准。
    Pending,
    /// 已批准，可以开始执行。
    Approved,
    /// 执行中。
    Running,
    /// 已暂停（超时/熔断/用户暂停），等待人工接管。
    Paused,
    /// 检测到敏感 UI，熔断暂停。
    Fused,
    /// 正常完成。
    Completed,
    /// 用户拒绝。
    Rejected,
    /// 用户取消。
    Cancelled,
}

impl TaskState {
    /// 是否允许执行桌面操作。
    pub fn can_execute(self) -> bool {
        matches!(self, TaskState::Approved | TaskState::Running)
    }

    /// 是否为终态（不再变化）。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskState::Completed | TaskState::Rejected | TaskState::Cancelled
        )
    }
}

/// computer-use 任务（文档 §7.3 任务级审批：目标应用 + 任务描述 + 最长时长 + 允许动作）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputerTask {
    pub id: String,
    /// 目标应用（进程名/白名单名，如 qq / notepad / msedge）。
    pub target_app: String,
    /// 任务描述（用户可见，审批依据）。
    pub description: String,
    /// 允许动作集（desktop_click / desktop_type / desktop_shortcut / desktop_launch / desktop_scroll …）。
    pub allowed_actions: Vec<String>,
    /// 最长执行时长（毫秒）；超时自动暂停。
    pub max_duration_ms: u64,
    pub state: TaskState,
    pub created_at: String,
    /// 熔断原因（Fused/Paused 时）。
    pub fuse_reason: Option<String>,
}

/// computer-use 任务注册表（进程内，AppState 持有）。
#[derive(Clone, Default)]
pub struct ComputerTaskRegistry {
    tasks: Arc<Mutex<HashMap<String, ComputerTask>>>,
    started: Arc<Mutex<HashMap<String, Instant>>>,
    /// 每个任务已执行的受控动作次数（预算消耗）。
    actions: Arc<Mutex<HashMap<String, u32>>>,
    /// 每个任务的动作次数预算（未设置时用 [`DEFAULT_ACTION_BUDGET`]；显式 0 = 不限）。
    action_budget: Arc<Mutex<HashMap<String, u32>>>,
}

impl ComputerTaskRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create(&self, task: ComputerTask) -> Result<(), String> {
        let mut tasks = self.tasks.lock().map_err(|_| "任务表锁中毒".to_string())?;
        if tasks.contains_key(&task.id) {
            return Err(format!("任务 {} 已存在", task.id));
        }
        tasks.insert(task.id.clone(), task);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<ComputerTask> {
        self.tasks
            .lock()
            .ok()
            .and_then(|tasks| tasks.get(id).cloned())
    }

    pub fn list(&self) -> Vec<ComputerTask> {
        self.tasks
            .lock()
            .map(|tasks| {
                let mut all: Vec<ComputerTask> = tasks.values().cloned().collect();
                all.sort_by(|a, b| b.created_at.cmp(&a.created_at));
                all
            })
            .unwrap_or_default()
    }

    /// 批准任务：Pending → Approved。只有 Pending 可批准。
    pub fn approve(&self, id: &str) -> Result<TaskState, String> {
        self.transition(id, |task| {
            if task.state != TaskState::Pending {
                return Err(format!(
                    "任务 {id} 当前状态 {:?}，只有 Pending 可批准",
                    task.state
                ));
            }
            Ok(TaskState::Approved)
        })
    }

    /// 拒绝任务：Pending → Rejected。
    pub fn reject(&self, id: &str) -> Result<TaskState, String> {
        self.transition(id, |task| {
            if task.state != TaskState::Pending {
                return Err(format!(
                    "任务 {id} 当前状态 {:?}，只有 Pending 可拒绝",
                    task.state
                ));
            }
            Ok(TaskState::Rejected)
        })
    }

    pub fn cancel(&self, id: &str) -> Result<TaskState, String> {
        self.transition(id, |task| {
            if task.state.is_terminal() {
                return Err(format!("任务 {id} 已结束（{:?}）", task.state));
            }
            Ok(TaskState::Cancelled)
        })
    }

    /// 开始执行：Approved → Running（记录起始时间，用于超时判定）。
    pub fn start(&self, id: &str) -> Result<TaskState, String> {
        let state = self.transition(id, |task| {
            if !task.state.can_execute() {
                return Err(format!(
                    "任务 {id} 状态 {:?} 不可执行（需先批准）",
                    task.state
                ));
            }
            Ok(TaskState::Running)
        })?;
        if let Ok(mut started) = self.started.lock() {
            started.insert(id.to_string(), Instant::now());
        }
        Ok(state)
    }

    /// 暂停（用户/超时/异常）：Running/Approved → Paused。
    pub fn pause(&self, id: &str, reason: &str) -> Result<TaskState, String> {
        self.transition(id, |task| {
            if task.state.is_terminal() {
                return Err(format!("任务 {id} 已结束（{:?}）", task.state));
            }
            task.fuse_reason = Some(reason.to_string());
            Ok(TaskState::Paused)
        })
    }

    /// 熔断（敏感 UI）：任何非终态 → Fused，要求人工接管。
    pub fn fuse(&self, id: &str, reason: &str) -> Result<TaskState, String> {
        self.transition(id, |task| {
            if task.state.is_terminal() {
                return Err(format!("任务 {id} 已结束（{:?}）", task.state));
            }
            task.fuse_reason = Some(reason.to_string());
            Ok(TaskState::Fused)
        })
    }

    /// 人工接管后恢复：Fused/Paused → Approved（可继续执行）。
    pub fn resume(&self, id: &str) -> Result<TaskState, String> {
        self.transition(id, |task| {
            if !matches!(task.state, TaskState::Fused | TaskState::Paused) {
                return Err(format!(
                    "任务 {id} 状态 {:?} 不可恢复（仅 Fused/Paused 可恢复）",
                    task.state
                ));
            }
            task.fuse_reason = None;
            Ok(TaskState::Approved)
        })
    }

    /// 完成：Running → Completed。
    pub fn complete(&self, id: &str) -> Result<TaskState, String> {
        self.transition(id, |task| {
            if !task.state.can_execute() {
                return Err(format!("任务 {id} 状态 {:?} 不可完成", task.state));
            }
            Ok(TaskState::Completed)
        })
    }

    /// 检查任务是否可执行（状态 + 超时）。超时自动暂停并返回错误。
    pub fn check_can_execute(&self, id: &str) -> Result<(), String> {
        let task = self
            .tasks
            .lock()
            .map_err(|_| "任务表锁中毒".to_string())?
            .get(id)
            .cloned()
            .ok_or_else(|| format!("任务 {id} 不存在"))?;
        if !task.state.can_execute() {
            return Err(format!("任务 {id} 状态 {:?}，不可执行", task.state));
        }
        if let Ok(started) = self.started.lock() {
            if let Some(when) = started.get(id) {
                if when.elapsed() > Duration::from_millis(task.max_duration_ms) {
                    drop(started);
                    let _ = self.pause(
                        id,
                        &format!("执行超时（超过最长时长 {}ms）", task.max_duration_ms),
                    );
                    return Err(format!(
                        "任务 {id} 执行超时（超过最长时长 {}ms），已自动暂停",
                        task.max_duration_ms
                    ));
                }
            }
        }
        Ok(())
    }

    /// 校验动作是否在任务允许集内（拒绝未声明动作）。
    pub fn check_action_allowed(&self, id: &str, action: &str) -> Result<(), String> {
        let task = self
            .tasks
            .lock()
            .map_err(|_| "任务表锁中毒".to_string())?
            .get(id)
            .cloned()
            .ok_or_else(|| format!("任务 {id} 不存在"))?;
        if task.allowed_actions.is_empty() {
            // 空允许集 = 未声明动作清单（老任务兼容），不限制；文档要求声明则收紧。
            return Ok(());
        }
        if task.allowed_actions.iter().any(|allowed| allowed == action) {
            Ok(())
        } else {
            Err(format!(
                "动作 {action} 不在任务 {id} 允许集内（{:?}）",
                task.allowed_actions
            ))
        }
    }

    /// 校验动作次数预算；超限自动暂停并返回错误（m4d 动作预算上限）。
    /// 设置动作次数预算（显式 0 = 不限；未设置时默认 [`DEFAULT_ACTION_BUDGET`]）。
    pub fn set_action_budget(&self, id: &str, budget: u32) -> Result<(), String> {
        let exists = {
            let tasks = self.tasks.lock().map_err(|_| "任务表锁中毒".to_string())?;
            tasks.contains_key(id)
        };
        if !exists {
            return Err(format!("任务 {id} 不存在"));
        }
        if let Ok(mut budgets) = self.action_budget.lock() {
            budgets.insert(id.to_string(), budget);
        }
        Ok(())
    }

    /// 查询任务已执行动作数。
    pub fn actions_taken(&self, id: &str) -> u32 {
        self.actions
            .lock()
            .ok()
            .and_then(|actions| actions.get(id).copied())
            .unwrap_or(0)
    }

    /// 查询任务当前预算（未设置返回 [`DEFAULT_ACTION_BUDGET`]）。
    pub fn action_budget(&self, id: &str) -> u32 {
        self.action_budget
            .lock()
            .ok()
            .and_then(|budgets| budgets.get(id).copied())
            .unwrap_or(DEFAULT_ACTION_BUDGET)
    }

    pub fn check_action_budget(&self, id: &str) -> Result<(), String> {
        let exists = {
            let tasks = self.tasks.lock().map_err(|_| "任务表锁中毒".to_string())?;
            tasks.contains_key(id)
        };
        if !exists {
            return Err(format!("任务 {id} 不存在"));
        }
        let cap = self.action_budget(id);
        if cap == 0 {
            return Ok(());
        }
        let count = self.actions_taken(id);
        if count >= cap {
            drop(self.started.lock());
            let _ = self.pause(
                id,
                &format!("动作次数超预算（上限 {cap} 次，已执行 {count} 次）"),
            );
            return Err(format!(
                "任务 {id} 动作次数超预算（上限 {cap} 次），已自动暂停"
            ));
        }
        Ok(())
    }

    /// 记录一次已执行动作（预算计数，仅递增不校验；校验走 `check_action_budget`）。
    pub fn record_action(&self, id: &str) {
        if let Ok(mut actions) = self.actions.lock() {
            let entry = actions.entry(id.to_string()).or_insert(0);
            *entry = entry.saturating_add(1);
        }
    }

    /// 目标应用是否匹配任务声明（大小写不敏感等值匹配，`*.exe` 后缀等价；`*` 或 `any` 视为通配）。
    pub fn target_matches(&self, id: &str, actual_app: &str) -> Result<bool, String> {
        let task = self
            .tasks
            .lock()
            .map_err(|_| "任务表锁中毒".to_string())?
            .get(id)
            .cloned()
            .ok_or_else(|| format!("任务 {id} 不存在"))?;
        let expected = task.target_app.trim().to_lowercase();
        let actual = actual_app.trim().to_lowercase();
        let actual_base = actual
            .strip_suffix(".exe")
            .unwrap_or(actual.as_str())
            .trim()
            .to_string();
        Ok(expected == "*" || expected == "any" || expected == actual || expected == actual_base)
    }

    fn transition<F>(&self, id: &str, f: F) -> Result<TaskState, String>
    where
        F: FnOnce(&mut ComputerTask) -> Result<TaskState, String>,
    {
        let mut tasks = self.tasks.lock().map_err(|_| "任务表锁中毒".to_string())?;
        let task = tasks
            .get_mut(id)
            .ok_or_else(|| format!("任务 {id} 不存在"))?;
        let next = f(task)?;
        task.state = next;
        Ok(next)
    }
}

/// 敏感 UI 检测（熔断）：对 UI 树节点/OCR 文本做关键词扫描（大小写不敏感）。
/// 命中即表示当前界面存在密码/支付/验证码等敏感控件，computer-use 应熔断。
/// 纯函数（可测）；关键词与 `action_program::sensitive_anchor` 保持同一集合。
pub fn sensitive_ui_hit(name: &str, role: &str, ocr_text: &str) -> Option<String> {
    const KEYWORDS: [&str; 9] = [
        "password",
        "密码",
        "支付",
        "付款",
        "验证码",
        "captcha",
        "card",
        "卡号",
        "登录",
    ];
    let combined = format!(
        "{} {} {}",
        name.to_lowercase(),
        role.to_lowercase(),
        ocr_text.to_lowercase()
    );
    for keyword in KEYWORDS {
        if combined.contains(keyword) {
            return Some(format!("检测到敏感界面元素：{keyword}"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_task(id: &str) -> ComputerTask {
        ComputerTask {
            id: id.to_string(),
            target_app: "notepad".to_string(),
            description: "打开记事本输入文本".to_string(),
            allowed_actions: vec!["desktop_click".to_string(), "desktop_type".to_string()],
            max_duration_ms: 60_000,
            state: TaskState::Pending,
            created_at: "2026-08-13T00:00:00Z".to_string(),
            fuse_reason: None,
        }
    }

    #[test]
    fn lifecycle_pending_approve_run_complete() {
        let registry = ComputerTaskRegistry::new();
        registry.create(sample_task("t1")).unwrap();
        assert_eq!(registry.get("t1").unwrap().state, TaskState::Pending);
        // 未批准不可执行。
        assert!(registry.check_can_execute("t1").is_err());
        assert_eq!(registry.approve("t1").unwrap(), TaskState::Approved);
        assert_eq!(registry.start("t1").unwrap(), TaskState::Running);
        assert!(registry.check_can_execute("t1").is_ok());
        assert_eq!(registry.complete("t1").unwrap(), TaskState::Completed);
        assert!(registry.check_can_execute("t1").is_err());
    }

    #[test]
    fn fuse_and_resume_require_manual_takeover() {
        let registry = ComputerTaskRegistry::new();
        registry.create(sample_task("t2")).unwrap();
        registry.approve("t2").unwrap();
        registry.start("t2").unwrap();
        let reason = registry.fuse("t2", "检测到密码框").unwrap();
        assert_eq!(reason, TaskState::Fused);
        assert!(registry.check_can_execute("t2").is_err());
        // 熔断后必须人工接管（resume）才能继续。
        assert_eq!(registry.resume("t2").unwrap(), TaskState::Approved);
        assert!(registry.check_can_execute("t2").is_ok());
    }

    #[test]
    fn reject_and_cancel_are_terminal() {
        let registry = ComputerTaskRegistry::new();
        registry.create(sample_task("t3")).unwrap();
        assert_eq!(registry.reject("t3").unwrap(), TaskState::Rejected);
        assert!(registry.approve("t3").is_err());

        registry.create(sample_task("t4")).unwrap();
        registry.approve("t4").unwrap();
        assert_eq!(registry.cancel("t4").unwrap(), TaskState::Cancelled);
        assert!(registry.start("t4").is_err());
    }

    #[test]
    fn action_allowlist_enforced() {
        let registry = ComputerTaskRegistry::new();
        registry.create(sample_task("t5")).unwrap();
        assert!(registry.check_action_allowed("t5", "desktop_click").is_ok());
        assert!(registry.check_action_allowed("t5", "desktop_type").is_ok());
        let denied = registry.check_action_allowed("t5", "desktop_launch");
        assert!(denied.is_err());
        assert!(denied.unwrap_err().contains("允许集"));
    }

    #[test]
    fn timeout_pauses_automatically() {
        let registry = ComputerTaskRegistry::new();
        let mut task = sample_task("t6");
        task.max_duration_ms = 10;
        registry.create(task).unwrap();
        registry.approve("t6").unwrap();
        registry.start("t6").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let error = registry.check_can_execute("t6").unwrap_err();
        assert!(error.contains("超时"));
        assert_eq!(registry.get("t6").unwrap().state, TaskState::Paused);
    }

    #[test]
    fn sensitive_ui_detects_keywords() {
        assert!(sensitive_ui_hit("PasswordBox", "", "").is_some());
        assert!(sensitive_ui_hit("", "Edit", "请输入支付密码").is_some());
        assert!(sensitive_ui_hit("", "", "captcha 验证码").is_some());
        assert!(sensitive_ui_hit("输入消息", "Edit", "").is_none());
        assert!(sensitive_ui_hit("发送", "Button", "发送消息").is_none());
    }

    #[test]
    fn action_budget_pauses_when_exceeded() {
        let registry = ComputerTaskRegistry::new();
        registry.create(sample_task("b1")).unwrap();
        registry.set_action_budget("b1", 2).unwrap();
        registry.approve("b1").unwrap();
        registry.start("b1").unwrap();
        assert!(registry.check_action_budget("b1").is_ok());
        registry.record_action("b1");
        assert!(registry.check_action_budget("b1").is_ok());
        registry.record_action("b1");
        let error = registry.check_action_budget("b1").unwrap_err();
        assert!(error.contains("超预算"));
        assert_eq!(registry.get("b1").unwrap().state, TaskState::Paused);
    }

    #[test]
    fn action_budget_default_and_unlimited_semantics() {
        let registry = ComputerTaskRegistry::new();
        registry.create(sample_task("d1")).unwrap();
        assert_eq!(registry.action_budget("d1"), DEFAULT_ACTION_BUDGET);
        registry.set_action_budget("d1", 0).unwrap();
        assert_eq!(registry.action_budget("d1"), 0);
        for _ in 0..(DEFAULT_ACTION_BUDGET + 10) {
            registry.record_action("d1");
        }
        // 显式 0 = 不限，永不超预算。
        assert!(registry.check_action_budget("d1").is_ok());
        assert_eq!(registry.actions_taken("d1"), DEFAULT_ACTION_BUDGET + 10);
    }

    #[test]
    fn target_matches_wildcard_and_case_insensitive() {
        let registry = ComputerTaskRegistry::new();
        registry.create(sample_task("m1")).unwrap();
        assert!(registry.target_matches("m1", "notepad").unwrap());
        assert!(registry.target_matches("m1", "Notepad").unwrap());
        assert!(registry.target_matches("m1", "notepad.exe").unwrap());
        assert!(!registry.target_matches("m1", "qq").unwrap());
        let mut task = sample_task("m2");
        task.target_app = "*".to_string();
        registry.create(task).unwrap();
        assert!(registry.target_matches("m2", "anything").unwrap());
    }

    #[test]
    fn computer_task_serde_roundtrip_without_budget_field() {
        // 预算不落在任务结构体上（registry 侧维护），旧 JSON 可无损反序列化。
        let old_json = r#"{"id":"s1","target_app":"notepad","description":"d","allowed_actions":[],"max_duration_ms":1000,"state":"Pending","created_at":"2026-08-13T00:00:00Z","fuse_reason":null}"#;
        let task: ComputerTask = serde_json::from_str(old_json).unwrap();
        let encoded = serde_json::to_string(&task).unwrap();
        let round: ComputerTask = serde_json::from_str(&encoded).unwrap();
        assert_eq!(round.id, "s1");
        assert_eq!(round.max_duration_ms, 1000);
    }
}
