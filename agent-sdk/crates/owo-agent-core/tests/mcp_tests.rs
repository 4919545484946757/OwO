use owo_agent_core::audit::AuditLog;
use owo_agent_core::mcp::{McpClient, McpServerConfig};
use owo_agent_core::permissions::Policy;
use owo_agent_core::session::Session;
use owo_agent_core::skill::SkillRegistry;
use owo_agent_core::tools::{ToolContext, ToolRegistry};
use serde_json::json;
use std::sync::Arc;

fn test_config() -> McpServerConfig {
    McpServerConfig::stdio(
        "test",
        env!("CARGO_BIN_EXE_owo-mcp-test-server"),
        Vec::new(),
    )
}

#[tokio::test]
async fn mcp_stdio_connect_list_and_call() {
    let mut client = McpClient::connect(&test_config()).await.unwrap();
    let tools = client.tools();
    assert!(tools.iter().any(|tool| tool.name == "echo"));
    assert!(tools.iter().any(|tool| tool.name == "add"));

    let echo = client
        .call_tool("echo", json!({ "text": "你好" }))
        .await
        .unwrap();
    assert_eq!(echo["text"], "你好");

    let add = client
        .call_tool("add", json!({ "a": 1, "b": 2 }))
        .await
        .unwrap();
    assert_eq!(add["text"], "3");

    let unknown = client.call_tool("nope", json!({})).await;
    assert!(unknown.is_err());
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn mcp_tools_are_registered_and_callable() {
    let client = Arc::new(tokio::sync::Mutex::new(
        McpClient::connect(&test_config()).await.unwrap(),
    ));
    let tools = client.lock().await.tools();
    let mut registry = ToolRegistry::new();
    registry.register_mcp_tools("test", Arc::clone(&client), tools);

    let specs = registry.specs();
    assert!(specs.iter().any(|spec| spec.name == "test_echo"));

    let workspace =
        std::env::temp_dir().join(format!("owo-mcp-registry-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&workspace).unwrap();
    let mut session = Session::new(&workspace, "mock".to_string(), None);
    let audit = Arc::new(std::sync::Mutex::new(AuditLog::default()));
    let policy = Policy::new(&workspace);
    let skills = SkillRegistry::default();
    let mut context = ToolContext {
        workspace: &workspace,
        policy: &policy,
        session: &mut session,
        audit: &audit,
        subagent: None,
        skills: &skills,
    };
    let result = registry
        .execute("test_echo", &mut context, json!({ "text": "ok" }))
        .await
        .unwrap();
    assert_eq!(result["text"], "ok");
    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn mcp_http_connect_list_and_call() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut server = tokio::process::Command::new(env!("CARGO_BIN_EXE_owo-mcp-http-test-server"))
        .arg(port.to_string())
        .spawn()
        .unwrap();

    let config = McpServerConfig::http("http-test", format!("http://127.0.0.1:{port}/mcp"));
    let mut client = None;
    for _attempt in 0..10 {
        match McpClient::connect(&config).await {
            Ok(connected) => {
                client = Some(connected);
                break;
            }
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
        }
    }
    let mut client = client.expect("HTTP MCP 服务器应可连接");

    let tools = client.tools();
    assert!(tools.iter().any(|tool| tool.name == "echo"));
    let echo = client
        .call_tool("echo", json!({ "text": "http-ok" }))
        .await
        .unwrap();
    assert_eq!(echo["text"], "http-ok");
    client.shutdown().await.unwrap();
    let _ = server.kill().await;
}
