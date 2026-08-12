//! MCP（Model Context Protocol）客户端：stdio 与 HTTP 双传输，JSON-RPC 2.0。

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE};
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

fn default_transport() -> String {
    "stdio".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    /// "stdio" 或 "http"
    #[serde(default = "default_transport")]
    pub transport: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// HTTP 传输时的端点 URL
    #[serde(default)]
    pub url: Option<String>,
}

impl McpServerConfig {
    pub fn stdio(name: impl Into<String>, command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            name: name.into(),
            transport: "stdio".to_string(),
            command: command.into(),
            args,
            url: None,
        }
    }

    pub fn http(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            transport: "http".to_string(),
            command: String::new(),
            args: Vec::new(),
            url: Some(url.into()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

struct StdioTransport {
    child: Child,
    stdin: ChildStdin,
    pending: Pending,
    next_id: AtomicU64,
}

enum Transport {
    Stdio(Box<StdioTransport>),
    Http {
        client: reqwest::Client,
        url: String,
        headers: HeaderMap,
        next_id: AtomicU64,
    },
}

pub struct McpClient {
    transport: Transport,
    tools: Vec<McpTool>,
}

impl McpClient {
    /// 启动/连接服务器并完成握手（initialize → initialized → tools/list）。
    pub async fn connect(config: &McpServerConfig) -> Result<Self, String> {
        let transport = if config.transport == "http" {
            let url = config
                .url
                .clone()
                .ok_or_else(|| "HTTP MCP 服务器缺少 url".to_string())?;
            let client = reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(60))
                .build()
                .map_err(|error| format!("HTTP 客户端创建失败：{error}"))?;
            let mut headers = HeaderMap::new();
            headers.insert(
                ACCEPT,
                HeaderValue::from_static("application/json, text/event-stream"),
            );
            Transport::Http {
                client,
                url,
                headers,
                next_id: AtomicU64::new(1),
            }
        } else {
            let mut command = Command::new(&config.command);
            command
                .args(&config.args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::inherit());
            let mut child = command
                .spawn()
                .map_err(|error| format!("MCP 服务器启动失败（{}）：{error}", config.command))?;
            let stdin = child
                .stdin
                .take()
                .ok_or_else(|| "MCP 服务器无 stdin".to_string())?;
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| "MCP 服务器无 stdout".to_string())?;

            let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
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
            Transport::Stdio(Box::new(StdioTransport {
                child,
                stdin,
                pending,
                next_id: AtomicU64::new(1),
            }))
        };

        let mut client = Self {
            transport,
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
            "MCP 服务器 {}（{server_version}，{}）已连接，工具 {} 个",
            config.name,
            config.transport,
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
            Ok(json!({
                "text": text,
                "content": result.get("content").cloned().unwrap_or(Value::Null)
            }))
        }
    }

    pub async fn shutdown(&mut self) -> Result<(), String> {
        if matches!(&self.transport, Transport::Stdio(_)) {
            let _ = self.notify("exit", json!({})).await;
        }
        match &mut self.transport {
            Transport::Stdio(transport) => {
                let child = &mut transport.child;
                child.kill().await.map_err(|error| error.to_string())?;
                let _ = child.wait().await;
            }
            Transport::Http { .. } => {
                // HTTP 传输无显式 exit；连接随客户端释放。
            }
        }
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        match &mut self.transport {
            Transport::Stdio(transport) => {
                request_stdio(
                    &mut transport.stdin,
                    &transport.pending,
                    &transport.next_id,
                    method,
                    params,
                )
                .await
            }
            Transport::Http {
                client,
                url,
                headers,
                next_id,
            } => request_http(client, url, headers, next_id, method, params).await,
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        match &mut self.transport {
            Transport::Stdio(transport) => {
                let stdin = &mut transport.stdin;
                let message = json!({
                    "jsonrpc": "2.0",
                    "method": method,
                    "params": params,
                });
                let payload = serde_json::to_string(&message).map_err(|error| error.to_string())?;
                stdin
                    .write_all(payload.as_bytes())
                    .await
                    .map_err(|error| format!("MCP 写入失败：{error}"))?;
                stdin
                    .write_all(b"\n")
                    .await
                    .map_err(|error| format!("MCP 写入失败：{error}"))?;
                stdin
                    .flush()
                    .await
                    .map_err(|error| format!("MCP 刷新失败：{error}"))
            }
            Transport::Http {
                client,
                url,
                headers,
                ..
            } => {
                let message = json!({
                    "jsonrpc": "2.0",
                    "method": method,
                    "params": params,
                });
                let _ = client
                    .post(url.as_str())
                    .headers(headers.clone())
                    .json(&message)
                    .send()
                    .await;
                Ok(())
            }
        }
    }
}

async fn request_stdio(
    stdin: &mut ChildStdin,
    pending: &Pending,
    next_id: &AtomicU64,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let id = next_id.fetch_add(1, Ordering::Relaxed);
    let (sender, receiver) = oneshot::channel();
    pending
        .lock()
        .map_err(|_| "MCP 响应表锁中毒".to_string())?
        .insert(id, sender);
    let message = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    let payload = serde_json::to_string(&message).map_err(|error| error.to_string())?;
    stdin
        .write_all(payload.as_bytes())
        .await
        .map_err(|error| format!("MCP 写入失败：{error}"))?;
    stdin
        .write_all(b"\n")
        .await
        .map_err(|error| format!("MCP 写入失败：{error}"))?;
    stdin
        .flush()
        .await
        .map_err(|error| format!("MCP 刷新失败：{error}"))?;
    let response = tokio::time::timeout(Duration::from_secs(30), receiver)
        .await
        .map_err(|_| format!("MCP 请求超时：{method}"))?
        .map_err(|_| "MCP 服务器已关闭".to_string())?;
    extract_result(response)
}

async fn request_http(
    client: &reqwest::Client,
    url: &str,
    headers: &HeaderMap,
    next_id: &AtomicU64,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let id = next_id.fetch_add(1, Ordering::Relaxed);
    let message = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    let response = client
        .post(url)
        .headers(headers.clone())
        .json(&message)
        .send()
        .await
        .map_err(|error| format!("MCP HTTP 请求失败：{error}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response
            .text()
            .await
            .unwrap_or_else(|_| "无响应体".to_string());
        return Err(format!("MCP HTTP 返回 {status}：{text}"));
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_lowercase();
    if content_type.contains("text/event-stream") {
        let text = response
            .text()
            .await
            .map_err(|error| format!("MCP SSE 读取失败：{error}"))?;
        for line in text.lines() {
            if let Some(payload) = line.trim().strip_prefix("data:") {
                if payload.trim() == "[DONE]" {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<Value>(payload) {
                    match extract_result(value) {
                        Ok(result) => return Ok(result),
                        Err(error) => return Err(error),
                    }
                }
            }
        }
        Err(format!("MCP SSE 无有效响应：{method}"))
    } else {
        let value: Value = response
            .json()
            .await
            .map_err(|error| format!("MCP HTTP 响应解析失败：{error}"))?;
        extract_result(value)
    }
}

fn extract_result(value: Value) -> Result<Value, String> {
    if let Some(error) = value.get("error") {
        return Err(format!("MCP 错误：{error}"));
    }
    Ok(value.get("result").cloned().unwrap_or(Value::Null))
}
