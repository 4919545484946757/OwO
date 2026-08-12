use crate::tools::ToolSpec;
use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn system(content: String) -> Self {
        Self {
            role: "system".into(),
            content: Some(content),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn user(content: String) -> Self {
        Self {
            role: "user".into(),
            content: Some(content),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant_text(content: String) -> Self {
        Self {
            role: "assistant".into(),
            content: Some(content),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant_tool_calls(tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
        }
    }

    pub fn tool(tool_call_id: String, content: String) -> Self {
        Self {
            role: "tool".into(),
            content: Some(content),
            tool_calls: None,
            tool_call_id: Some(tool_call_id),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ModelOutput {
    Text(String),
    ToolCalls(Vec<ToolCall>),
}

#[async_trait]
pub trait ModelProvider: Send + Sync {
    async fn complete(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<ModelOutput, String>;

    /// 流式补全：文本增量经 `on_delta` 回调；返回最终输出。
    /// 默认实现退化为非流式。
    async fn complete_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<ModelOutput, String> {
        let output = self.complete(messages, tools).await?;
        if let ModelOutput::Text(text) = &output {
            on_delta(text.clone());
        }
        Ok(output)
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiCompatibleConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// 数据出境开关：false 时拒绝一切云端模型调用。
    pub cloud_enabled: bool,
}

impl OpenAiCompatibleConfig {
    pub fn from_env() -> Result<Self, String> {
        let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| {
            "缺少 OPENAI_API_KEY 环境变量（或设置 OPENAI_BASE_URL 指向本地兼容端点）".to_string()
        })?;
        let base_url = std::env::var("OPENAI_BASE_URL")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
        let model =
            std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".to_string());
        let cloud_enabled = std::env::var("OWO_CLOUD_ENABLED")
            .ok()
            .and_then(|value| value.parse::<bool>().ok())
            .unwrap_or(true);
        Ok(Self {
            base_url,
            api_key,
            model,
            cloud_enabled,
        })
    }
}

/// OpenAI-compatible `/chat/completions` 客户端（覆盖 OpenAI、DeepSeek、Ollama、多数代理）。
pub struct OpenAiCompatibleProvider {
    client: reqwest::Client,
    config: OpenAiCompatibleConfig,
}

impl OpenAiCompatibleProvider {
    pub fn new(config: OpenAiCompatibleConfig) -> Result<Self, String> {
        let mut builder = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(180));
        for name in [
            "OWO_HTTP_PROXY",
            "HTTPS_PROXY",
            "HTTP_PROXY",
            "https_proxy",
            "http_proxy",
        ] {
            if let Ok(proxy) = std::env::var(name) {
                if !proxy.trim().is_empty() {
                    let proxy = reqwest::Proxy::all(proxy)
                        .map_err(|e| format!("代理配置无效（{name}）：{e}"))?;
                    builder = builder.proxy(proxy);
                    break;
                }
            }
        }
        let client = builder
            .build()
            .map_err(|e| format!("HTTP 客户端创建失败：{e}"))?;
        Ok(Self { client, config })
    }

    fn request_body(&self, messages: &[ChatMessage], tools: &[ToolSpec], stream: bool) -> Value {
        let tool_payload: Vec<Value> = tools
            .iter()
            .map(|spec| {
                json!({
                    "type": "function",
                    "function": {
                        "name": spec.name,
                        "description": spec.description,
                        "parameters": spec.input_schema,
                    }
                })
            })
            .collect();
        let messages_payload: Vec<Value> = messages
            .iter()
            .map(|message| {
                let mut wire = json!({
                    "role": message.role,
                    "content": message.content,
                });
                if let Some(tool_call_id) = &message.tool_call_id {
                    wire["tool_call_id"] = Value::String(tool_call_id.clone());
                }
                if let Some(tool_calls) = &message.tool_calls {
                    let wire_calls: Vec<Value> = tool_calls
                        .iter()
                        .map(|call| {
                            json!({
                                "id": call.id,
                                "type": "function",
                                "function": {
                                    "name": call.name,
                                    "arguments": serde_json::to_string(&call.arguments)
                                        .unwrap_or_else(|_| "{}".to_string()),
                                }
                            })
                        })
                        .collect();
                    wire["tool_calls"] = Value::Array(wire_calls);
                }
                wire
            })
            .collect();

        let mut body = json!({
            "model": self.config.model,
            "messages": messages_payload,
            "stream": stream,
        });
        if !tool_payload.is_empty() {
            body["tools"] = Value::Array(tool_payload);
        }
        body
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct StreamDelta {
    pub content: Option<String>,
    /// 原始 tool_calls 增量片段（JSON 值）。
    pub tool_call_fragments: Vec<Value>,
}

/// 解析一条 `data:` 负载。空负载/心跳返回 None。
pub fn parse_sse_payload(payload: &str) -> Option<StreamDelta> {
    let payload = payload.trim();
    if payload.is_empty() || payload == "[DONE]" {
        return None;
    }
    let value: Value = serde_json::from_str(payload).ok()?;
    let delta = value.pointer("/choices/0/delta")?;
    let content = delta
        .get("content")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let tool_call_fragments = delta
        .get("tool_calls")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if content.is_none() && tool_call_fragments.is_empty() {
        return None;
    }
    Some(StreamDelta {
        content,
        tool_call_fragments,
    })
}

#[derive(Debug, Default)]
struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments: String,
}

fn accumulate_tool_fragments(
    accumulators: &mut HashMap<usize, ToolCallAccumulator>,
    fragments: &[Value],
) {
    for fragment in fragments {
        let Some(index) = fragment.get("index").and_then(Value::as_u64) else {
            continue;
        };
        let index = index as usize;
        let entry = accumulators.entry(index).or_default();
        if let Some(id) = fragment.get("id").and_then(Value::as_str) {
            entry.id = id.to_string();
        }
        if let Some(name) = fragment.pointer("/function/name").and_then(Value::as_str) {
            entry.name = name.to_string();
        }
        if let Some(arguments) = fragment
            .pointer("/function/arguments")
            .and_then(Value::as_str)
        {
            entry.arguments.push_str(arguments);
        }
    }
}

fn build_tool_calls(
    accumulators: &mut HashMap<usize, ToolCallAccumulator>,
) -> Option<Vec<ToolCall>> {
    if accumulators.is_empty() {
        return None;
    }
    let mut calls: Vec<(usize, ToolCall)> = accumulators
        .drain()
        .map(|(index, accum)| {
            (
                index,
                ToolCall {
                    id: if accum.id.is_empty() {
                        format!("call_{index}")
                    } else {
                        accum.id
                    },
                    name: accum.name,
                    arguments: serde_json::from_str(&accum.arguments).unwrap_or(Value::Null),
                },
            )
        })
        .collect();
    calls.sort_by_key(|(index, _)| *index);
    Some(calls.into_iter().map(|(_, call)| call).collect())
}

#[async_trait]
impl ModelProvider for OpenAiCompatibleProvider {
    async fn complete(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<ModelOutput, String> {
        if !self.config.cloud_enabled {
            return Err("云端模型已禁用（数据出境开关关闭）".to_string());
        }
        let body = self.request_body(messages, tools, false);
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.config.api_key)
            .json(&body)
            .timeout(std::time::Duration::from_secs(180))
            .send()
            .await
            .map_err(|e| format!("模型请求失败：{e}"))?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response
                .text()
                .await
                .unwrap_or_else(|_| "无响应体".to_string());
            return Err(format!("模型返回 {status}：{text}"));
        }

        let payload: Value = response
            .json()
            .await
            .map_err(|e| format!("模型响应解析失败：{e}"))?;
        let message = payload
            .pointer("/choices/0/message")
            .ok_or_else(|| "响应缺少 choices[0].message".to_string())?;
        let content = message
            .get("content")
            .and_then(Value::as_str)
            .map(str::to_string);
        let tool_calls = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .map(|calls| {
                calls
                    .iter()
                    .filter_map(|call| {
                        let id = call.get("id")?.as_str()?.to_string();
                        let name = call.pointer("/function/name")?.as_str()?.to_string();
                        let arguments = call
                            .pointer("/function/arguments")
                            .and_then(Value::as_str)
                            .and_then(|raw| serde_json::from_str(raw).ok())
                            .unwrap_or(Value::Null);
                        Some(ToolCall {
                            id,
                            name,
                            arguments,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|calls: &Vec<ToolCall>| !calls.is_empty());

        if let Some(tool_calls) = tool_calls {
            Ok(ModelOutput::ToolCalls(tool_calls))
        } else if let Some(content) = content {
            Ok(ModelOutput::Text(content))
        } else {
            Err("模型响应既无文本也无工具调用".to_string())
        }
    }

    async fn complete_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<ModelOutput, String> {
        if !self.config.cloud_enabled {
            return Err("云端模型已禁用（数据出境开关关闭）".to_string());
        }
        let body = self.request_body(messages, tools, true);
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.config.api_key)
            .json(&body)
            .timeout(std::time::Duration::from_secs(180))
            .send()
            .await
            .map_err(|e| format!("模型请求失败：{e}"))?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response
                .text()
                .await
                .unwrap_or_else(|_| "无响应体".to_string());
            return Err(format!("模型返回 {status}：{text}"));
        }

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut content = String::new();
        let mut accumulators: HashMap<usize, ToolCallAccumulator> = HashMap::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("流式读取失败：{e}"))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            let mut completed = true;
            while completed {
                match buffer.find('\n') {
                    Some(newline) => {
                        let line = buffer[..newline].trim().to_string();
                        buffer.drain(..=newline);
                        if let Some(payload) = line.strip_prefix("data:") {
                            if payload.trim() == "[DONE]" {
                                break;
                            }
                            if let Some(delta) = parse_sse_payload(payload) {
                                if let Some(delta_content) = delta.content {
                                    content.push_str(&delta_content);
                                    on_delta(delta_content);
                                }
                                accumulate_tool_fragments(
                                    &mut accumulators,
                                    &delta.tool_call_fragments,
                                );
                            }
                        }
                    }
                    None => completed = false,
                }
            }
        }

        if let Some(tool_calls) = build_tool_calls(&mut accumulators) {
            Ok(ModelOutput::ToolCalls(tool_calls))
        } else {
            Ok(ModelOutput::Text(content))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_delta() {
        let delta = parse_sse_payload(r#"{"choices":[{"delta":{"content":"你好"}}]}"#).unwrap();
        assert_eq!(delta.content.as_deref(), Some("你好"));
        assert!(delta.tool_call_fragments.is_empty());
    }

    #[test]
    fn parses_tool_call_fragments_and_assembles() {
        let delta = parse_sse_payload(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":"{\"path\":"}}]}}]}"#,
        )
        .unwrap();
        assert_eq!(delta.content, None);
        assert_eq!(delta.tool_call_fragments.len(), 1);

        let mut accumulators = HashMap::new();
        accumulate_tool_fragments(&mut accumulators, &delta.tool_call_fragments);
        let delta2 = parse_sse_payload(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.txt\"}"}}]}}]}"#,
        )
        .unwrap();
        accumulate_tool_fragments(&mut accumulators, &delta2.tool_call_fragments);

        let calls = build_tool_calls(&mut accumulators).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments["path"], "a.txt");
    }

    #[test]
    fn ignores_heartbeat_and_done() {
        assert!(parse_sse_payload("").is_none());
        assert!(parse_sse_payload("[DONE]").is_none());
        assert!(parse_sse_payload(": keep-alive").is_none());
    }

    #[test]
    fn cloud_disabled_rejects_requests_before_network() {
        std::env::set_var("OPENAI_API_KEY", "test");
        std::env::set_var("OPENAI_BASE_URL", "http://127.0.0.1:9");
        std::env::set_var("OPENAI_MODEL", "mock");
        std::env::set_var("OWO_CLOUD_ENABLED", "false");
        let config = OpenAiCompatibleConfig::from_env().unwrap();
        assert!(!config.cloud_enabled);
        let provider = OpenAiCompatibleProvider::new(config).unwrap();
        let error = futures::executor::block_on(provider.complete(&[], &[])).unwrap_err();
        assert!(error.contains("数据出境"));
        std::env::remove_var("OWO_CLOUD_ENABLED");
    }
}
