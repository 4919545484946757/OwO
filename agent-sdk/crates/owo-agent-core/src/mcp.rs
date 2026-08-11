//! MCP（Model Context Protocol）客户端：stdio 传输，JSON-RPC 2.0。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::oneshot;

pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

pub struct McpClient {
    child: Child,
    stdin: ChildStdin,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    next_id: AtomicU64,
    tools: Vec<McpTool>,
}

impl McpClient {
    /// 启动子进程并完成握手（initialize → initialized → tools/list）。
    pub async fn connect(config: &McpServerConfig) -> Result<Self, String> {
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit());
        let mut child = command
            .spawn()
            .map_err(|e| format!("MCP 服务器启动失败（{}）：{e}", config.command))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "MCP 服务器无 stdin".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "MCP 服务器无 stdout".to_string())?;

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = Arc::clone(&pending);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                let Ok(Some(line)) = lines.next_line().await else {
                    break;
                };
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if let Some(id) = value.get("id").and_then(Value::as_u64) {
                    if let Ok(mut map) = reader_pending.lock() {
                        if let Some(sender) = map.remove(&id) {
                            let _ = sender.send(value);
                        }
                    }
                }
            }
        });

        let mut client = Self {
            child,
            stdin,
            pending,
            next_id: AtomicU64::new(1),
            tools: Vec::new(),
        };
        let initialize = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "owo-agent", "version": env!("CARGO_PKG_VERSION") },
                }),
            )
            .await?;
        let server_version = initialize
            .pointer("/serverInfo/name")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        client
            .notify("notifications/initialized", json!({}))
            .await?;
        let tools_result = client.request("tools/list", json!({})).await?;
        client.tools = tools_result
            .get("tools")
            .and_then(Value::as_array)
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|tool| {
                        Some(McpTool {
                            name: tool.get("name")?.as_str()?.to_string(),
                            description: tool
                                .get("description")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            input_schema: tool.get("inputSchema").cloned().unwrap_or(Value::Null),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        tracing::info!(
            "MCP 服务器 {}（{server_version}）已连接，工具 {} 个",
            config.name,
            client.tools.len()
        );
        Ok(client)
    }

    pub fn tools(&self) -> Vec<McpTool> {
        self.tools.clone()
    }

    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value, String> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let text = result
            .get("content")
            .and_then(Value::as_array)
            .map(|content| {
                content
                    .iter()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if is_error {
            Err(text)
        } else {
            Ok(
                json!({ "text": text, "content": result.get("content").cloned().unwrap_or(Value::Null) }),
            )
        }
    }

    pub async fn shutdown(&mut self) -> Result<(), String> {
        let _ = self.notify("exit", json!({})).await;
        self.child.kill().await.map_err(|e| e.to_string())?;
        let _ = self.child.wait().await;
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| "MCP 响应表锁中毒".to_string())?
            .insert(id, sender);
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let payload = serde_json::to_string(&message).map_err(|e| e.to_string())?;
        self.stdin
            .write_all(payload.as_bytes())
            .await
            .map_err(|e| format!("MCP 写入失败：{e}"))?;
        self.stdin
            .write_all(b"\n")
            .await
            .map_err(|e| format!("MCP 写入失败：{e}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| format!("MCP 刷新失败：{e}"))?;

        let response = tokio::time::timeout(Duration::from_secs(30), receiver)
            .await
            .map_err(|_| format!("MCP 请求超时：{method}"))?
            .map_err(|_| "MCP 服务器已关闭".to_string())?;
        if let Some(error) = response.get("error") {
            return Err(format!("MCP 错误：{error}"));
        }
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        let message = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let payload = serde_json::to_string(&message).map_err(|e| e.to_string())?;
        self.stdin
            .write_all(payload.as_bytes())
            .await
            .map_err(|e| format!("MCP 写入失败：{e}"))?;
        self.stdin
            .write_all(b"\n")
            .await
            .map_err(|e| format!("MCP 写入失败：{e}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| format!("MCP 刷新失败：{e}"))
    }
}
