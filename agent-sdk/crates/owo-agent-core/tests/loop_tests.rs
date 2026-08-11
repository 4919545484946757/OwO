use async_trait::async_trait;
use owo_agent_core::permissions::{AutoApprover, Policy};
use owo_agent_core::skill::SkillRegistry;
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

struct StreamingProvider {
    output: String,
}

struct RecordingProvider {
    script: Mutex<VecDeque<ModelOutput>>,
    recorded: Arc<Mutex<Vec<ChatMessage>>>,
}

#[async_trait]
impl ModelProvider for RecordingProvider {
    async fn complete(
        &self,
        messages: &[ChatMessage],
        _tools: &[ToolSpec],
    ) -> Result<ModelOutput, String> {
        self.recorded
            .lock()
            .unwrap()
            .extend(messages.iter().cloned());
        self.script
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| "脚本输出耗尽".to_string())
    }
}

#[async_trait]
impl ModelProvider for StreamingProvider {
    async fn complete(
        &self,
        _messages: &[ChatMessage],
        _tools: &[ToolSpec],
    ) -> Result<ModelOutput, String> {
        Ok(ModelOutput::Text(self.output.clone()))
    }

    async fn complete_stream(
        &self,
        _messages: &[ChatMessage],
        _tools: &[ToolSpec],
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<ModelOutput, String> {
        for character in self.output.chars() {
            on_delta(character.to_string());
        }
        Ok(ModelOutput::Text(self.output.clone()))
    }
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

fn build_agent<P>(workspace: &std::path::Path, provider: P) -> Agent
where
    P: ModelProvider + Send + Sync + 'static,
{
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

#[tokio::test]
async fn streaming_deltas_are_emitted_and_final_text_returned() {
    let workspace = temp_workspace("streaming");
    let provider = StreamingProvider {
        output: "你好".to_string(),
    };
    let agent = build_agent(&workspace, provider);
    let mut session = Session::new(&workspace, "mock".to_string(), None);
    let abort = AtomicBool::new(false);
    let approver = AutoApprover { allow: true };

    let outcome = agent
        .run_turn(&mut session, "hi", &approver, &abort, &mut |_| {})
        .await
        .unwrap();

    assert_eq!(outcome.final_text.as_deref(), Some("你好"));
    let deltas: Vec<String> = outcome
        .events
        .iter()
        .filter_map(|event| match event {
            owo_agent_core::TurnEvent::TokenDelta { delta } => Some(delta.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, vec!["你".to_string(), "好".to_string()]);
}

#[tokio::test]
async fn explore_subagent_runs_read_only_child_and_returns_report() {
    let workspace = temp_workspace("subagent-explore");
    std::fs::write(workspace.join("info.txt"), "重要信息").unwrap();
    let provider = ScriptedProvider::new(vec![
        call("c1", "explore", json!({ "query": "info.txt 的内容" })),
        ModelOutput::Text("子代理发现：重要信息".to_string()),
        ModelOutput::Text("done".to_string()),
    ]);
    let agent = build_agent(&workspace, provider);
    let mut session = Session::new(&workspace, "mock".to_string(), None);
    let abort = AtomicBool::new(false);
    let approver = AutoApprover { allow: true };

    let outcome = agent
        .run_turn(&mut session, "探索一下", &approver, &abort, &mut |_| {})
        .await
        .unwrap();

    assert_eq!(outcome.final_text.as_deref(), Some("done"));
    assert!(session.messages.iter().any(|message| message
        .content
        .as_deref()
        .is_some_and(|c| c.contains("子代理发现"))));
}

#[tokio::test]
async fn read_only_registry_has_no_write_or_delegate_tools() {
    let registry = ToolRegistry::read_only();
    let specs = registry.specs();
    assert!(specs.iter().any(|spec| spec.name == "read_file"));
    assert!(specs.iter().all(|spec| {
        !matches!(
            spec.name.as_str(),
            "write_file" | "run_command" | "explore" | "subagent"
        )
    }));
}

#[tokio::test]
async fn skills_are_discovered_injected_and_usable() {
    let workspace = temp_workspace("skills");
    let skill_dir = workspace.join(".agents").join("skills").join("demo");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: demo\ndescription: 测试技能\n---\n执行步骤 A。",
    )
    .unwrap();
    let data_root =
        std::env::temp_dir().join(format!("owo-skill-loop-data-{}", uuid::Uuid::new_v4()));
    let registry = SkillRegistry::discover(&workspace, &data_root);
    assert!(registry.get("demo").is_some());

    let recorded = Arc::new(Mutex::new(Vec::new()));
    let provider = RecordingProvider {
        script: Mutex::new(
            vec![
                call("c1", "use_skill", json!({ "name": "demo" })),
                ModelOutput::Text("done".to_string()),
            ]
            .into(),
        ),
        recorded: Arc::clone(&recorded),
    };
    let mut agent = build_agent(&workspace, provider);
    agent.set_skills(registry);
    let mut session = Session::new(&workspace, "mock".to_string(), None);
    let abort = AtomicBool::new(false);
    let approver = AutoApprover { allow: true };

    let outcome = agent
        .run_turn(&mut session, "使用技能", &approver, &abort, &mut |_| {})
        .await
        .unwrap();
    assert_eq!(outcome.final_text.as_deref(), Some("done"));

    let messages = recorded.lock().unwrap();
    let system = messages
        .iter()
        .find(|message| message.role == "system")
        .expect("存在系统消息");
    let system = system.content.as_deref().unwrap();
    assert!(system.contains("可用技能"));
    assert!(system.contains("demo"));
    drop(messages);

    let tool_result = session
        .messages
        .iter()
        .find(|message| message.role == "tool")
        .expect("存在工具结果");
    assert!(tool_result
        .content
        .as_deref()
        .unwrap()
        .contains("执行步骤 A"));
    let _ = std::fs::remove_dir_all(&data_root);
}
