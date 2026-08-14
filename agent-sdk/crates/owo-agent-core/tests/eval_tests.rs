use async_trait::async_trait;
use owo_agent_core::{
    run_suite, ChatMessage, EvalCase, ModelOutput, ModelProvider, ToolCall, ToolSpec,
};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

struct KeywordProvider;

#[async_trait]
impl ModelProvider for KeywordProvider {
    async fn complete(
        &self,
        messages: &[ChatMessage],
        _tools: &[ToolSpec],
    ) -> Result<ModelOutput, String> {
        let text = messages
            .iter()
            .filter_map(|message| message.content.clone())
            .collect::<Vec<_>>()
            .join("\n");
        let output = if text.contains("读取") {
            "内容：hello-eval".to_string()
        } else if text.contains("创建") {
            "已创建 eval-ok".to_string()
        } else if text.contains("列出") {
            "src.txt".to_string()
        } else if text.contains("搜索") {
            "todo.md".to_string()
        } else if text.contains("explore") {
            "subagent-notes".to_string()
        } else {
            "其他结果".to_string()
        };
        Ok(ModelOutput::Text(output))
    }
}

#[tokio::test]
async fn eval_suite_reports_pass_and_fail() {
    let mut builtin = owo_agent_core::builtin_suite();
    let mut suite = owo_agent_core::EvalSuite {
        name: "test".to_string(),
        cases: builtin.cases.drain(..5).collect(),
    };
    suite.cases.push(EvalCase {
        name: "fail_case".to_string(),
        prompt: "做点别的".to_string(),
        expected: vec!["不存在".to_string()],
        setup_files: Vec::new(),
        expected_files: Vec::new(),
        expected_missing: Vec::new(),
    });

    let report = run_suite(Arc::new(KeywordProvider), "mock", &suite).await;

    assert_eq!(report.total, 6);
    assert_eq!(report.passed, 5);
    assert!(report.pass_rate > 0.8);
    assert!(
        !report
            .cases
            .iter()
            .find(|case| case.name == "fail_case")
            .unwrap()
            .passed
    );
    let read = report
        .cases
        .iter()
        .find(|case| case.name == "read_file")
        .unwrap();
    assert!(read.passed);
    assert!(read.output.contains("hello-eval"));
}

#[test]
fn builtin_suite_has_at_least_thirty_cases() {
    assert!(owo_agent_core::builtin_suite().cases.len() >= 30);
}

/// 脚本化 Provider：按序吐出工具调用/最终文本（验证 expected_files/expected_missing 语义）。
struct ScriptedProvider {
    script: Mutex<VecDeque<ModelOutput>>,
}

impl ScriptedProvider {
    fn new(outputs: Vec<ModelOutput>) -> Self {
        Self {
            script: Mutex::new(outputs.into()),
        }
    }
}

fn call(id: &str, name: &str, args: serde_json::Value) -> ModelOutput {
    ModelOutput::ToolCalls(vec![ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments: args,
    }])
}

#[async_trait]
impl ModelProvider for ScriptedProvider {
    async fn complete(
        &self,
        _messages: &[ChatMessage],
        _tools: &[ToolSpec],
    ) -> Result<ModelOutput, String> {
        self.script
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| "脚本输出耗尽".to_string())
    }
}

#[tokio::test]
async fn expected_files_and_missing_are_enforced() {
    // ① 真实写文件 + expected_files → 通过
    let mut suite = owo_agent_core::EvalSuite {
        name: "files".to_string(),
        cases: vec![EvalCase {
            name: "real_write".to_string(),
            prompt: "创建 verify.txt".to_string(),
            expected: vec!["好了".to_string()],
            setup_files: Vec::new(),
            expected_files: vec![("verify.txt".to_string(), "verified".to_string())],
            expected_missing: Vec::new(),
        }],
    };
    let provider = ScriptedProvider::new(vec![
        call(
            "t1",
            "write_file",
            json!({ "path": "verify.txt", "content": "verified-content" }),
        ),
        ModelOutput::Text("好了".to_string()),
    ]);
    let report = run_suite(Arc::new(provider), "mock", &suite).await;
    assert!(
        report.cases[0].passed,
        "真实落盘应通过：{:?}",
        report.cases[0].error
    );

    // ② 模型口头声称写了但未落盘 → 失败且错误指明期望文件
    suite.cases[0].name = "fake_write".to_string();
    let provider = ScriptedProvider::new(vec![ModelOutput::Text(
        "已创建 verify.txt，内容 verified".to_string(),
    )]);
    let report = run_suite(Arc::new(provider), "mock", &suite).await;
    assert!(!report.cases[0].passed, "未落盘应失败");
    assert!(
        report.cases[0]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("期望文件未就绪")),
        "错误应指明期望文件：{:?}",
        report.cases[0].error
    );

    // ③ 删除文件 + expected_missing → 通过
    suite.cases[0] = EvalCase {
        name: "real_delete".to_string(),
        prompt: "删除 old.txt".to_string(),
        expected: vec!["好了".to_string()],
        setup_files: vec![("old.txt".to_string(), "x".to_string())],
        expected_files: Vec::new(),
        expected_missing: vec!["old.txt".to_string()],
    };
    let provider = ScriptedProvider::new(vec![
        call("t2", "run_command", json!({ "command": "del old.txt" })),
        ModelOutput::Text("好了".to_string()),
    ]);
    let report = run_suite(Arc::new(provider), "mock", &suite).await;
    assert!(
        report.cases[0].passed,
        "删除成功应通过：{:?}",
        report.cases[0].error
    );

    // ④ 未删除 → 失败且错误指明残留文件
    suite.cases[0].name = "fake_delete".to_string();
    let provider = ScriptedProvider::new(vec![ModelOutput::Text("已删除 old.txt".to_string())]);
    let report = run_suite(Arc::new(provider), "mock", &suite).await;
    assert!(!report.cases[0].passed, "未删除应失败");
    assert!(
        report.cases[0]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("应被删除的文件仍存在")),
        "错误应指明残留文件：{:?}",
        report.cases[0].error
    );
}
