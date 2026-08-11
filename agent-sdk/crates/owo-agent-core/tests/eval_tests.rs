use async_trait::async_trait;
use owo_agent_core::{run_suite, ChatMessage, EvalCase, ModelOutput, ModelProvider, ToolSpec};
use std::sync::Arc;

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
fn builtin_suite_has_at_least_twenty_cases() {
    assert!(owo_agent_core::builtin_suite().cases.len() >= 20);
}
