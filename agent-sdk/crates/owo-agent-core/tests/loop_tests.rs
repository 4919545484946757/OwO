use async_trait::async_trait;
use owo_agent_core::permissions::{AutoApprover, Policy};
use owo_agent_core::tools::ToolRegistry;
use owo_agent_core::{
    Agent, AgentConfig, ChatMessage, ModelOutput, ModelProvider, Session, ToolCall, ToolSpec,
};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

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

fn call(id: &str, name: &str, args: serde_json::Value) -> ModelOutput {
    ModelOutput::ToolCalls(vec![ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments: args,
    }])
}

fn temp_workspace(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("owo-agent-test-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn build_agent(workspace: &std::path::Path, provider: ScriptedProvider) -> Agent {
    let policy = Policy::new(workspace.to_path_buf());
    let registry = ToolRegistry::new();
    Agent::new(Arc::new(provider), registry, policy, AgentConfig::default())
}

#[tokio::test]
async fn read_write_finish_closed_loop() {
    let workspace = temp_workspace("closed-loop");
    std::fs::write(workspace.join("a.txt"), "hello\n").unwrap();
    let provider = ScriptedProvider::new(vec![
        call("c1", "read_file", json!({ "path": "a.txt" })),
        call(
            "c2",
            "write_file",
            json!({ "path": "b.txt", "content": "world\n" }),
        ),
        ModelOutput::Text("done".to_string()),
    ]);
    let agent = build_agent(&workspace, provider);
    let mut session = Session::new(&workspace, "mock".to_string(), None);
    let abort = AtomicBool::new(false);
    let approver = AutoApprover { allow: true };

    let outcome = agent
        .run_turn(&mut session, "write b.txt", &approver, &abort, &mut |_| {})
        .await
        .unwrap();

    assert_eq!(outcome.final_text.as_deref(), Some("done"));
    assert_eq!(outcome.steps, 2);
    assert_eq!(
        std::fs::read_to_string(workspace.join("b.txt")).unwrap(),
        "world\n"
    );
    let audit = agent.audit_log();
    let audit = audit.lock().unwrap();
    assert!(audit.entries.iter().any(|e| e.event == "tool_call"));
    assert!(audit.entries.iter().any(|e| e.approved == Some(true)));
}

#[tokio::test]
async fn denied_write_does_not_touch_workspace() {
    let workspace = temp_workspace("deny-write");
    let provider = ScriptedProvider::new(vec![
        call(
            "c1",
            "write_file",
            json!({ "path": "b.txt", "content": "x" }),
        ),
        ModelOutput::Text("ok".to_string()),
    ]);
    let agent = build_agent(&workspace, provider);
    let mut session = Session::new(&workspace, "mock".to_string(), None);
    let abort = AtomicBool::new(false);
    let approver = AutoApprover { allow: false };

    let outcome = agent
        .run_turn(&mut session, "write", &approver, &abort, &mut |_| {})
        .await
        .unwrap();

    assert_eq!(outcome.final_text.as_deref(), Some("ok"));
    assert!(!workspace.join("b.txt").exists());
    let audit = agent.audit_log();
    let audit = audit.lock().unwrap();
    assert!(audit.entries.iter().any(|e| e.approved == Some(false)));
}

#[tokio::test]
async fn path_outside_workspace_is_denied() {
    let workspace = temp_workspace("scope");
    let outside = temp_workspace("scope-outside");
    std::fs::write(outside.join("secret.txt"), "secret").unwrap();
    let provider = ScriptedProvider::new(vec![
        call(
            "c1",
            "read_file",
            json!({ "path": outside.join("secret.txt").to_string_lossy() }),
        ),
        ModelOutput::Text("ok".to_string()),
    ]);
    let agent = build_agent(&workspace, provider);
    let mut session = Session::new(&workspace, "mock".to_string(), None);
    let abort = AtomicBool::new(false);
    let approver = AutoApprover { allow: true };

    let outcome = agent
        .run_turn(&mut session, "read secret", &approver, &abort, &mut |_| {})
        .await
        .unwrap();

    assert_eq!(outcome.final_text.as_deref(), Some("ok"));
    let audit = agent.audit_log();
    let audit = audit.lock().unwrap();
    assert!(audit
        .entries
        .iter()
        .any(|e| e.approved == Some(false) && e.detail.contains("工作区之外")));
}

#[tokio::test]
async fn revert_restores_original_content() {
    let workspace = temp_workspace("revert");
    std::fs::write(workspace.join("a.txt"), "original\n").unwrap();
    let provider = ScriptedProvider::new(vec![
        call(
            "c1",
            "write_file",
            json!({ "path": "a.txt", "content": "changed\n" }),
        ),
        ModelOutput::Text("done".to_string()),
    ]);
    let agent = build_agent(&workspace, provider);
    let mut session = Session::new(&workspace, "mock".to_string(), None);
    let abort = AtomicBool::new(false);
    let approver = AutoApprover { allow: true };

    agent
        .run_turn(&mut session, "change a.txt", &approver, &abort, &mut |_| {})
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(workspace.join("a.txt")).unwrap(),
        "changed\n"
    );
    assert_eq!(session.diff().len(), 1);

    let restored = session.revert().await.unwrap();
    assert_eq!(restored.len(), 1);
    assert_eq!(
        std::fs::read_to_string(workspace.join("a.txt")).unwrap(),
        "original\n"
    );
    assert!(session.diff().is_empty());
}

#[tokio::test]
async fn revert_removes_created_file() {
    let workspace = temp_workspace("revert-created");
    let provider = ScriptedProvider::new(vec![
        call(
            "c1",
            "write_file",
            json!({ "path": "new.txt", "content": "created\n" }),
        ),
        ModelOutput::Text("done".to_string()),
    ]);
    let agent = build_agent(&workspace, provider);
    let mut session = Session::new(&workspace, "mock".to_string(), None);
    let abort = AtomicBool::new(false);
    let approver = AutoApprover { allow: true };

    agent
        .run_turn(
            &mut session,
            "create new.txt",
            &approver,
            &abort,
            &mut |_| {},
        )
        .await
        .unwrap();
    assert!(workspace.join("new.txt").exists());

    let restored = session.revert().await.unwrap();
    assert_eq!(restored, vec!["new.txt"]);
    assert!(!workspace.join("new.txt").exists());
    assert!(session.diff().is_empty());
}
