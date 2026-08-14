//! 动作程序（v0.5 M-B，对应技术文档 5.8.3）。
//!
//! 从线性动作图升级为可分支/循环/等待/重试的动作程序：
//! `ProgramNode` 支持 Step/Assert/WaitUntil/Branch/Loop/Retry/Sub。
//! 执行器为解释器 + 状态机：每步 = 敏感面熔断 → 执行 → 断言 → 记录；
//! 旧线性 `graph.json` 自动转换为 `Vec<Step>` 兼容。

use crate::assert::{describe, verify_assertion_full, Assertion};
use crate::executor::{parse_click_at, ExecReport, ExecStep, UiActionSource};
use crate::learn::{ActionGraph, ActionType, SemanticAnchor};
use crate::ocr::OcrSummary;
use crate::perception::SituationSnapshot;
use crate::scene::SceneGraph;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 动作程序节点：结构化控制流。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProgramNode {
    Step {
        id: String,
        action: ActionType,
        anchor: SemanticAnchor,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value_template: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verify: Option<Assertion>,
    },
    Assert {
        id: String,
        assertion: Assertion,
    },
    WaitUntil {
        id: String,
        assertion: Assertion,
        timeout_ms: u64,
    },
    Branch {
        id: String,
        cond: Assertion,
        then: Vec<ProgramNode>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        otherwise: Vec<ProgramNode>,
    },
    Loop {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cond: Option<Assertion>,
        body: Vec<ProgramNode>,
        max_iter: u32,
    },
    Retry {
        id: String,
        body: Vec<ProgramNode>,
        max_attempts: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_fail: Option<Vec<ProgramNode>>,
    },
    Sub {
        id: String,
        program: String,
    },
}

/// 动作程序：版本化节点序列（解释器按控制流执行）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionProgram {
    pub version: u32,
    pub name: String,
    pub nodes: Vec<ProgramNode>,
}

impl ActionProgram {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            version: 1,
            name: name.into(),
            nodes: Vec::new(),
        }
    }

    pub fn push(&mut self, node: ProgramNode) {
        self.nodes.push(node);
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

/// 断言评估上下文：情景快照 + OCR 摘要 + 场景图。
#[derive(Debug, Clone, Copy, Default)]
pub struct ProgramContext<'a> {
    pub snapshot: Option<&'a SituationSnapshot>,
    pub ocr: Option<&'a OcrSummary>,
    pub scene: Option<&'a SceneGraph>,
}

/// 线性动作图 → 动作程序（旧 `.owskill`/`graph.json` 自动兼容）。
pub fn from_graph(graph: &ActionGraph) -> ActionProgram {
    let mut program = ActionProgram::new("converted");
    let mut ids = std::collections::HashSet::new();
    let mut next_id = 0usize;
    for node in &graph.nodes {
        let id = if ids.insert(node.id.clone()) {
            node.id.clone()
        } else {
            next_id += 1;
            format!("converted-{}", next_id)
        };
        program.push(ProgramNode::Step {
            id,
            action: node.action_type,
            anchor: node.anchor.clone(),
            value_template: node.value_template.clone(),
            verify: None,
        });
    }
    program
}

/// 执行动作程序（无子程序、无感知上下文；需要断言的程序请用带上下文版本）。
pub fn execute_program(
    source: &dyn UiActionSource,
    program: &ActionProgram,
    variables: &HashMap<String, String>,
    max_steps: usize,
) -> ExecReport {
    execute_program_with_subprograms(source, program, &HashMap::new(), variables, max_steps)
}

/// 执行动作程序（支持 Sub 子程序分发；断言需要实时感知上下文时用带上下文版本）。
pub fn execute_program_with_subprograms(
    source: &dyn UiActionSource,
    program: &ActionProgram,
    programs: &HashMap<String, ActionProgram>,
    variables: &HashMap<String, String>,
    max_steps: usize,
) -> ExecReport {
    execute_program_with_context(
        source,
        program,
        programs,
        variables,
        ProgramContext::default(),
        max_steps,
    )
}

/// 完整解释器：控制流 + 子程序 + 结构化断言（v0.5 M-B 最终语义）。
pub fn execute_program_with_context(
    source: &dyn UiActionSource,
    program: &ActionProgram,
    programs: &HashMap<String, ActionProgram>,
    variables: &HashMap<String, String>,
    context: ProgramContext<'_>,
    max_steps: usize,
) -> ExecReport {
    let mut interpreter = Interpreter {
        source,
        variables,
        context,
        steps: Vec::new(),
        steps_taken: 0,
        max_steps: max_steps.max(1),
        depth: 0,
    };
    let run = interpreter.run_nodes(&program.nodes, programs);
    let error = run.err();
    ExecReport {
        ok: error.is_none(),
        steps: interpreter.steps,
        error,
    }
}

struct Interpreter<'a> {
    source: &'a dyn UiActionSource,
    variables: &'a HashMap<String, String>,
    context: ProgramContext<'a>,
    steps: Vec<ExecStep>,
    steps_taken: usize,
    max_steps: usize,
    depth: usize,
}

impl Interpreter<'_> {
    fn run_nodes(
        &mut self,
        nodes: &[ProgramNode],
        programs: &HashMap<String, ActionProgram>,
    ) -> Result<(), String> {
        for node in nodes {
            self.step(node, programs)?;
        }
        Ok(())
    }

    fn step(
        &mut self,
        node: &ProgramNode,
        programs: &HashMap<String, ActionProgram>,
    ) -> Result<(), String> {
        if self.steps_taken >= self.max_steps {
            return Err(format!("动作程序超过步数上限：{}", self.max_steps));
        }
        self.steps_taken += 1;
        match node {
            ProgramNode::Step {
                id,
                action,
                anchor,
                value_template,
                verify,
            } => {
                let text = value_template
                    .as_deref()
                    .map(|template| self.fill(template))
                    .unwrap_or_default();
                let (mut ok, mut detail) = match self.perform(*action, anchor, &text) {
                    Ok(()) => (true, String::new()),
                    Err(error) => (false, error),
                };
                if ok {
                    if let Some(assertion) = verify {
                        match self.eval(assertion) {
                            Ok(true) => {}
                            Ok(false) => {
                                ok = false;
                                detail = format!("验证失败：{}", describe(assertion));
                            }
                            Err(error) => {
                                ok = false;
                                detail = error;
                            }
                        }
                    }
                }
                let action_label = format!("{:?}", action).to_lowercase();
                self.record(id, &action_label, anchor.name.clone(), ok, &detail);
                if ok {
                    Ok(())
                } else {
                    Err(detail)
                }
            }
            ProgramNode::Assert { id, assertion } => {
                let description = describe(assertion);
                match self.eval(assertion) {
                    Ok(true) => {
                        self.record(id, "assert", description, true, "断言通过");
                        Ok(())
                    }
                    Ok(false) => {
                        let detail = format!("断言失败：{description}");
                        self.record(id, "assert", description, false, &detail);
                        Err(detail)
                    }
                    Err(error) => {
                        self.record(id, "assert", description, false, &error);
                        Err(error)
                    }
                }
            }
            ProgramNode::WaitUntil {
                id,
                assertion,
                timeout_ms,
            } => {
                let description = describe(assertion);
                let deadline =
                    std::time::Instant::now() + std::time::Duration::from_millis(*timeout_ms);
                loop {
                    match self.eval(assertion) {
                        Ok(true) => {
                            self.record(id, "wait_until", description, true, "条件满足");
                            return Ok(());
                        }
                        Ok(false) => {}
                        Err(error) => {
                            self.record(id, "wait_until", description, false, &error);
                            return Err(error);
                        }
                    }
                    if std::time::Instant::now() >= deadline {
                        let detail = format!("等待超时（{timeout_ms}ms）：{description}");
                        self.record(id, "wait_until", description, false, &detail);
                        return Err(detail);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            }
            ProgramNode::Branch {
                id,
                cond,
                then,
                otherwise,
            } => {
                let description = describe(cond);
                match self.eval(cond) {
                    Ok(true) => {
                        self.run_nodes(then, programs)?;
                        self.record(id, "branch", description, true, "走 then 分支");
                        Ok(())
                    }
                    Ok(false) => {
                        self.run_nodes(otherwise, programs)?;
                        self.record(id, "branch", description, true, "走 otherwise 分支");
                        Ok(())
                    }
                    Err(error) => {
                        self.record(id, "branch", description, false, &error);
                        Err(error)
                    }
                }
            }
            ProgramNode::Loop {
                id,
                cond,
                body,
                max_iter,
            } => {
                let description = cond
                    .as_ref()
                    .map(describe)
                    .unwrap_or_else(|| "无条件循环".to_string());
                let mut iterations = 0u32;
                loop {
                    if let Some(cond) = cond {
                        match self.eval(cond) {
                            Ok(true) => {}
                            Ok(false) => break,
                            Err(error) => {
                                self.record(id, "loop", description, false, &error);
                                return Err(error);
                            }
                        }
                    } else if iterations > 0 {
                        break;
                    }
                    if iterations >= *max_iter {
                        break;
                    }
                    self.run_nodes(body, programs)?;
                    iterations += 1;
                }
                let detail = format!("迭代 {iterations} 次");
                self.record(id, "loop", description, true, &detail);
                Ok(())
            }
            ProgramNode::Retry {
                id,
                body,
                max_attempts,
                on_fail,
            } => {
                let mut last_error = String::new();
                for attempt in 1..=(*max_attempts).max(1) {
                    match self.run_nodes(body, programs) {
                        Ok(()) => {
                            let detail = format!("第 {attempt} 次尝试成功");
                            self.record(id, "retry", "retry".to_string(), true, &detail);
                            return Ok(());
                        }
                        Err(error) => {
                            last_error = error;
                            if let Some(on_fail) = on_fail {
                                if let Err(error) = self.run_nodes(on_fail, programs) {
                                    last_error =
                                        format!("失败处理也失败：{error}（原错误：{last_error}）");
                                }
                            }
                        }
                    }
                }
                let detail = format!("重试 {max_attempts} 次后仍失败：{last_error}");
                self.record(id, "retry", "retry".to_string(), false, &detail);
                Err(detail)
            }
            ProgramNode::Sub { id, program: name } => {
                if self.depth >= 4 {
                    let detail = format!("子程序嵌套过深：{name}");
                    self.record(id, "sub", name.to_string(), false, &detail);
                    return Err(detail);
                }
                let sub = programs
                    .get(name)
                    .ok_or_else(|| format!("子程序不存在：{name}"))?;
                self.depth += 1;
                let result = self.run_nodes(&sub.nodes, programs);
                self.depth -= 1;
                match result {
                    Ok(()) => {
                        self.record(id, "sub", name.to_string(), true, "子程序完成");
                        Ok(())
                    }
                    Err(error) => {
                        self.record(id, "sub", name.to_string(), false, &error);
                        Err(error)
                    }
                }
            }
        }
    }

    fn perform(
        &self,
        action: ActionType,
        anchor: &SemanticAnchor,
        text: &str,
    ) -> Result<(), String> {
        if sensitive_anchor(anchor) {
            return Err("敏感面熔断：密码/支付/验证码等场景不执行".to_string());
        }
        match action {
            ActionType::Click => self
                .source
                .find(anchor)
                .and_then(|handle| self.source.invoke(handle)),
            ActionType::Type => self
                .source
                .find(anchor)
                .and_then(|handle| self.source.type_text(handle, text)),
            ActionType::Shortcut => self.source.shortcut(text),
            ActionType::Inject => self.source.type_text(0, text),
            ActionType::Launch => self.source.launch(text),
            ActionType::ClickAt => {
                parse_click_at(text).and_then(|(x, y)| self.source.click_at(x, y))
            }
            ActionType::Wait => Ok(()),
            ActionType::Scroll
            | ActionType::Drag
            | ActionType::Hover
            | ActionType::RightClick
            | ActionType::DoubleClick => Err(format!("动作类型 {action:?} 尚未接入解释器执行源")),
            ActionType::Assert => {
                Err("Assert 动作应使用 Assert/WaitUntil 程序节点，而非 Step".to_string())
            }
        }
    }

    fn eval(&self, assertion: &Assertion) -> Result<bool, String> {
        verify_assertion_full(
            assertion,
            self.context
                .snapshot
                .ok_or_else(|| format!("缺少情景快照，无法评估断言：{}", describe(assertion)))?,
            self.context.ocr,
            self.context.scene,
        )
    }

    fn fill(&self, template: &str) -> String {
        let mut output = template.to_string();
        for (name, value) in self.variables {
            output = output.replace(&format!("{{{name}}}"), value);
        }
        output
    }

    fn record(&mut self, node_id: &str, action: &str, anchor: String, ok: bool, detail: &str) {
        self.steps.push(ExecStep {
            node_id: node_id.to_string(),
            action: action.to_string(),
            anchor,
            status: if ok {
                "ok".to_string()
            } else {
                "failed".to_string()
            },
            detail: detail.to_string(),
        });
    }
}

fn sensitive_anchor(anchor: &SemanticAnchor) -> bool {
    ["password", "支付", "密码", "验证码", "captcha", "card"]
        .iter()
        .any(|keyword| {
            anchor.name.contains(keyword)
                || anchor
                    .role
                    .as_deref()
                    .map(|role| role.contains(keyword))
                    .unwrap_or(false)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr::OcrBox;
    use crate::perception::{ForegroundApp, UiContext};
    use crate::scene::SceneGraph;

    struct FakeSource {
        find_ok: bool,
        invoke_ok: bool,
        fail_invokes: std::sync::Mutex<u32>,
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl FakeSource {
        fn new(invoke_ok: bool) -> Self {
            Self {
                find_ok: true,
                invoke_ok,
                fail_invokes: std::sync::Mutex::new(0),
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl UiActionSource for FakeSource {
        fn find(&self, anchor: &SemanticAnchor) -> Result<u64, String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("find:{}", anchor.name));
            if self.find_ok {
                Ok(42)
            } else {
                Err(format!("未找到：{}", anchor.name))
            }
        }

        fn invoke(&self, _handle: u64) -> Result<(), String> {
            self.calls.lock().unwrap().push("invoke".to_string());
            let mut fails = self.fail_invokes.lock().unwrap();
            if *fails > 0 {
                *fails -= 1;
                Err("模拟失败".to_string())
            } else if self.invoke_ok {
                Ok(())
            } else {
                Err("invoke 失败".to_string())
            }
        }

        fn type_text(&self, _handle: u64, text: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("type:{text}"));
            Ok(())
        }

        fn shortcut(&self, combo: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("shortcut:{combo}"));
            Ok(())
        }

        fn launch(&self, target: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("launch:{target}"));
            Ok(())
        }

        fn click_at(&self, x: i32, y: i32) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("click_at:{x},{y}"));
            Ok(())
        }

        fn verify(&self, _predicate: &str) -> Result<bool, String> {
            Ok(true)
        }
    }

    fn snapshot(title: &str, ui_names: &[&str]) -> SituationSnapshot {
        SituationSnapshot {
            foreground_app: Some(ForegroundApp {
                id: "qq".to_string(),
                title: title.to_string(),
            }),
            permission_level: "L1".to_string(),
            ui_context: Some(UiContext {
                window: "qq".to_string(),
                active_view: "main".to_string(),
                accessible: true,
                ui_tree: ui_names
                    .iter()
                    .map(|name| crate::UiNode {
                        name: name.to_string(),
                        control_type: 50_000,
                        class: "Button".to_string(),
                        depth: 1,
                        x: 0,
                        y: 0,
                        width: 80,
                        height: 30,
                    })
                    .collect(),
            }),
            content: None,
            task_hypothesis: None,
            recent_actions: Vec::new(),
            capture: None,
        }
    }

    fn click_step(id: &str, name: &str) -> ProgramNode {
        ProgramNode::Step {
            id: id.to_string(),
            action: ActionType::Click,
            anchor: SemanticAnchor {
                app_id: Some("qq".to_string()),
                role: Some("button".to_string()),
                name: name.to_string(),
                parent: None,
                element_id: None,
            },
            value_template: None,
            verify: None,
        }
    }

    #[test]
    fn from_graph_converts_linear_steps() {
        let mut graph = ActionGraph::new();
        graph.add_node(
            "type",
            ActionType::Type,
            SemanticAnchor {
                app_id: Some("qq".to_string()),
                role: Some("edit".to_string()),
                name: "输入消息".to_string(),
                parent: None,
                element_id: None,
            },
            Some("你好 {name}".to_string()),
            None,
        );
        graph.add_node(
            "send",
            ActionType::Click,
            SemanticAnchor {
                app_id: Some("qq".to_string()),
                role: Some("button".to_string()),
                name: "发送".to_string(),
                parent: None,
                element_id: None,
            },
            None,
            None,
        );
        let program = from_graph(&graph);
        assert_eq!(program.nodes.len(), 2);
        assert!(matches!(
            program.nodes[0],
            ProgramNode::Step {
                action: ActionType::Type,
                ..
            }
        ));
    }

    #[test]
    fn program_supports_control_flow_nodes() {
        let mut program = ActionProgram::new("send-file");
        program.push(ProgramNode::WaitUntil {
            id: "wait".to_string(),
            assertion: Assertion::WindowTitle {
                expected: "QQ".to_string(),
            },
            timeout_ms: 3_000,
        });
        program.push(ProgramNode::Branch {
            id: "branch".to_string(),
            cond: Assertion::UiaExists {
                role: Some("button".to_string()),
                name: "发送".to_string(),
            },
            then: vec![],
            otherwise: vec![],
        });
        program.push(ProgramNode::Retry {
            id: "retry".to_string(),
            body: vec![],
            max_attempts: 3,
            on_fail: None,
        });
        assert_eq!(program.nodes.len(), 3);
    }

    #[test]
    fn branch_executes_chosen_path() {
        let mut program = ActionProgram::new("branch-demo");
        program.push(ProgramNode::Branch {
            id: "b1".to_string(),
            cond: Assertion::UiaExists {
                role: Some("button".to_string()),
                name: "发送".to_string(),
            },
            then: vec![click_step("t1", "发送")],
            otherwise: vec![click_step("o1", "取消")],
        });
        let source = FakeSource::new(true);
        let context = ProgramContext {
            snapshot: Some(&snapshot("QQ", &["发送"])),
            ocr: None,
            scene: None,
        };
        let report = execute_program_with_context(
            &source,
            &program,
            &HashMap::new(),
            &HashMap::new(),
            context,
            10,
        );
        assert!(report.ok, "分支程序应成功：{:?}", report.error);
        assert!(report.steps.iter().any(|step| step.anchor == "发送"));
        assert!(!report.steps.iter().any(|step| step.anchor == "取消"));
    }

    #[test]
    fn loop_respects_max_iterations() {
        let mut program = ActionProgram::new("loop-demo");
        program.push(ProgramNode::Loop {
            id: "l1".to_string(),
            cond: Some(Assertion::WindowTitle {
                expected: "QQ".to_string(),
            }),
            body: vec![click_step("c1", "计数")],
            max_iter: 3,
        });
        let source = FakeSource::new(true);
        let context = ProgramContext {
            snapshot: Some(&snapshot("QQ", &[])),
            ocr: None,
            scene: None,
        };
        let report = execute_program_with_context(
            &source,
            &program,
            &HashMap::new(),
            &HashMap::new(),
            context,
            20,
        );
        assert!(report.ok, "循环程序应成功：{:?}", report.error);
        let invokes = source
            .calls()
            .into_iter()
            .filter(|call| call == "invoke")
            .count();
        assert_eq!(invokes, 3, "max_iter=3 应恰好执行 3 次");
    }

    #[test]
    fn retry_attempts_until_success() {
        let mut program = ActionProgram::new("retry-demo");
        program.push(ProgramNode::Retry {
            id: "r1".to_string(),
            body: vec![click_step("t1", "尝试")],
            max_attempts: 3,
            on_fail: None,
        });
        let source = FakeSource::new(true);
        *source.fail_invokes.lock().unwrap() = 2;
        let report = execute_program(&source, &program, &HashMap::new(), 10);
        assert!(report.ok, "第 3 次应成功：{:?}", report.error);
        let invokes = source
            .calls()
            .into_iter()
            .filter(|call| call == "invoke")
            .count();
        assert_eq!(invokes, 3);
    }

    #[test]
    fn retry_runs_on_fail_and_reports_failure() {
        let mut program = ActionProgram::new("retry-fail-demo");
        program.push(ProgramNode::Retry {
            id: "r1".to_string(),
            body: vec![click_step("t1", "尝试")],
            max_attempts: 2,
            on_fail: Some(vec![click_step("f1", "兜底")]),
        });
        let source = FakeSource::new(false);
        let report = execute_program(&source, &program, &HashMap::new(), 10);
        assert!(!report.ok, "始终失败应返回失败");
        let calls = source.calls();
        let fallbacks = calls
            .iter()
            .filter(|call| call.as_str() == "find:兜底")
            .count();
        assert_eq!(fallbacks, 2, "每次失败后都应执行兜底");
    }

    #[test]
    fn wait_until_times_out() {
        let mut program = ActionProgram::new("wait-demo");
        program.push(ProgramNode::WaitUntil {
            id: "w1".to_string(),
            assertion: Assertion::WindowTitle {
                expected: "微信".to_string(),
            },
            timeout_ms: 50,
        });
        let source = FakeSource::new(true);
        let context = ProgramContext {
            snapshot: Some(&snapshot("QQ", &[])),
            ocr: None,
            scene: None,
        };
        let report = execute_program_with_context(
            &source,
            &program,
            &HashMap::new(),
            &HashMap::new(),
            context,
            10,
        );
        assert!(!report.ok);
        assert!(report.error.unwrap_or_default().contains("超时"));
    }

    #[test]
    fn sub_program_dispatches() {
        let mut sub = ActionProgram::new("open-input");
        sub.push(click_step("s1", "输入消息"));
        let mut program = ActionProgram::new("main");
        program.push(ProgramNode::Sub {
            id: "sub1".to_string(),
            program: "open-input".to_string(),
        });
        let mut programs = HashMap::new();
        programs.insert("open-input".to_string(), sub);
        let source = FakeSource::new(true);
        let report =
            execute_program_with_subprograms(&source, &program, &programs, &HashMap::new(), 10);
        assert!(report.ok, "子程序应成功：{:?}", report.error);
        assert!(report.steps.iter().any(|step| step.anchor == "输入消息"));
    }

    #[test]
    fn placeholder_branch_flow_is_executable() {
        // 路线图验收：if 输入框空（占位符可见） then 点击输入 else retry。
        let ocr = OcrSummary {
            text: "输入消息...".to_string(),
            chars: 5,
            boxes: vec![OcrBox {
                text: "输入消息...".to_string(),
                x: 0,
                y: 0,
                width: 120,
                height: 24,
            }],
            provider: Some("test".to_string()),
        };
        let mut program = ActionProgram::new("empty-box-flow");
        program.push(ProgramNode::Branch {
            id: "b1".to_string(),
            cond: Assertion::OcrBoxGone {
                text: "输入消息...".to_string(),
            },
            then: vec![click_step("t1", "发送")],
            otherwise: vec![click_step("o1", "清空")],
        });
        let source = FakeSource::new(true);
        let context = ProgramContext {
            snapshot: Some(&snapshot("QQ", &[])),
            ocr: Some(&ocr),
            scene: Some(&SceneGraph::new()),
        };
        let report = execute_program_with_context(
            &source,
            &program,
            &HashMap::new(),
            &HashMap::new(),
            context,
            10,
        );
        assert!(report.ok, "占位符分支流程应可执行：{:?}", report.error);
        assert!(report.steps.iter().any(|step| step.anchor == "发送"));
    }

    #[test]
    fn sensitive_step_is_blocked() {
        let mut program = ActionProgram::new("sensitive-demo");
        program.push(ProgramNode::Step {
            id: "p1".to_string(),
            action: ActionType::Click,
            anchor: SemanticAnchor {
                app_id: Some("qq".to_string()),
                role: None,
                name: "支付".to_string(),
                parent: None,
                element_id: None,
            },
            value_template: None,
            verify: None,
        });
        let source = FakeSource::new(true);
        let report = execute_program(&source, &program, &HashMap::new(), 10);
        assert!(!report.ok);
        assert!(report.error.unwrap_or_default().contains("敏感面熔断"));
        assert!(source.calls().is_empty(), "敏感步骤不得触碰执行源");
    }
}
