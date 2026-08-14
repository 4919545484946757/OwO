use crate::audit::AuditLog;
use crate::autoreview::{ReviewVerdict, Reviewer};
use crate::context::{build_system_prompt, load_project_rules};
use crate::error::AgentError;
use crate::gateway::{ChatMessage, ModelOutput, ModelProvider, TokenUsage};
use crate::injection::sanitize_tool_result;
use crate::permissions::{Approver, Decision, PermissionRequest, Policy};
use crate::session::Session;
use crate::skill::SkillRegistry;
use crate::subagent::SubagentRunner;
use crate::tools::{ToolContext, ToolRegistry};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

const MAX_TOOL_RESULT_CHARS: usize = 50_000;

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub max_turns: usize,
    pub context_limit: usize,
    pub subagent_depth: usize,
    pub token_budget: usize,
    pub keep_recent: usize,
    pub compaction_enabled: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: 60,
            context_limit: 200,
            subagent_depth: 0,
            token_budget: 60_000,
            keep_recent: 20,
            compaction_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnEvent {
    ModelCall,
    TokenDelta {
        delta: String,
    },
    Compaction {
        summary: String,
    },
    PermissionRequest(PermissionRequest),
    ToolStart {
        id: String,
        tool: String,
    },
    ToolResult {
        id: String,
        tool: String,
        ok: bool,
        error: Option<String>,
    },
    Final {
        text: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnOutcome {
    pub final_text: Option<String>,
    pub steps: usize,
    pub events: Vec<TurnEvent>,
    pub prompt: String,
    pub started_at: String,
    pub duration_ms: u64,
    /// 本回合模型 token 用量增量（provider 累计快照差值）。
    #[serde(default)]
    pub usage: TokenUsage,
}

/// Agent 核心：执行循环 + 工具注册表 + 权限策略 + 审计。
pub struct Agent {
    provider: Arc<dyn ModelProvider>,
    /// 工具注册表（RwLock：MCP 服务器热连接/热卸载时无需重建 Agent）。
    registry: Arc<RwLock<ToolRegistry>>,
    /// 插件热卸载：已禁用工具前缀（模型不可见、直接调用被拒）。
    disabled_tool_prefixes: Arc<RwLock<HashSet<String>>>,
    /// MCP 客户端进程生命周期注册表（进程级热卸载/退出清理）。
    mcp_clients: Arc<crate::mcp::McpRegistry>,
    /// 独立审批模型（Auto-review）：Ask 先经审查链，Deny 不打扰用户。
    reviewer: Option<Arc<dyn Reviewer>>,
    policy: Policy,
    audit: Arc<Mutex<AuditLog>>,
    config: AgentConfig,
    skills: SkillRegistry,
    elements: Arc<Mutex<crate::ElementRegistry>>,
}

impl Agent {
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        registry: ToolRegistry,
        policy: Policy,
        config: AgentConfig,
    ) -> Self {
        Self {
            provider,
            registry: Arc::new(RwLock::new(registry)),
            disabled_tool_prefixes: Arc::new(RwLock::new(HashSet::new())),
            mcp_clients: Arc::new(crate::mcp::McpRegistry::new()),
            reviewer: None,
            policy,
            audit: Arc::new(Mutex::new(AuditLog::default())),
            config,
            skills: SkillRegistry::default(),
            elements: Arc::new(Mutex::new(crate::ElementRegistry::new())),
        }
    }

    pub fn set_skills(&mut self, skills: SkillRegistry) {
        self.skills = skills;
    }

    /// 设置独立审批模型（None 表示关闭 Auto-review，恢复纯人工审批）。
    pub fn set_reviewer(&mut self, reviewer: Option<Arc<dyn Reviewer>>) {
        self.reviewer = reviewer;
    }

    /// 当前是否启用 Auto-review。
    pub fn autoreview_enabled(&self) -> bool {
        self.reviewer.is_some()
    }

    /// 注册 MCP 服务器工具（命名空间 `{server}_{tool}`）；热连接，无需重建 Agent。
    pub fn register_mcp_tools(
        &self,
        server_name: &str,
        client: Arc<tokio::sync::Mutex<crate::mcp::McpClient>>,
        tools: Vec<crate::mcp::McpTool>,
    ) {
        self.mcp_clients.insert(server_name, Arc::clone(&client));
        if let Ok(mut registry) = self.registry.write() {
            registry.register_mcp_tools(server_name, client, tools);
        }
    }

    /// MCP 客户端进程注册表（进程级热卸载/状态查询）。
    pub fn mcp_clients(&self) -> Arc<crate::mcp::McpRegistry> {
        Arc::clone(&self.mcp_clients)
    }

    /// 热连接 MCP 服务器并注册工具（插件启用/热添加）；返回工具数。
    pub async fn connect_mcp_server(
        &self,
        config: &crate::mcp::McpServerConfig,
    ) -> Result<usize, String> {
        let client = crate::mcp::McpClient::connect(config).await?;
        let tools = client.tools();
        let tool_count = tools.len();
        self.register_mcp_tools(
            &config.name,
            Arc::new(tokio::sync::Mutex::new(client)),
            tools,
        );
        Ok(tool_count)
    }

    /// 进程级热卸载 MCP 服务器：kill stdio 子进程 + 撤销工具（前缀移除且禁用）。
    /// 返回 false 表示服务器本未连接（幂等，不报错）。
    pub async fn shutdown_mcp_server(&self, name: &str) -> Result<bool, String> {
        if !self.mcp_clients.names().iter().any(|n| n == name) {
            return Ok(false);
        }
        let prefix = crate::tools::mcp_tool_prefix(name);
        self.set_tool_prefix_enabled(&prefix, false);
        self.remove_tools_prefix(&prefix);
        self.mcp_clients.shutdown(name).await?;
        Ok(true)
    }

    /// 关闭全部 MCP 客户端（服务退出前调用，防止遗留 stdio 子进程）。
    pub async fn shutdown_all_mcp(&self) -> Vec<(String, String)> {
        self.mcp_clients.shutdown_all().await
    }

    /// 按前缀撤销工具（插件热卸载）；返回移除数量。
    pub fn remove_tools_prefix(&self, prefix: &str) -> usize {
        self.registry
            .write()
            .map(|mut registry| registry.remove_prefix(prefix))
            .unwrap_or(0)
    }

    /// 插件工具前缀启停（热卸载：模型不可见 + 直接调用被拒，无需重建 Agent）。
    pub fn set_tool_prefix_enabled(&self, prefix: &str, enabled: bool) {
        if let Ok(mut prefixes) = self.disabled_tool_prefixes.write() {
            if enabled {
                prefixes.remove(prefix);
            } else {
                prefixes.insert(prefix.to_string());
            }
        }
    }

    /// 工具名是否命中任一禁用前缀。
    pub fn tool_disabled(&self, name: &str) -> bool {
        self.disabled_tool_prefixes
            .read()
            .map(|prefixes| prefixes.iter().any(|prefix| name.starts_with(prefix)))
            .unwrap_or(false)
    }

    /// 当前模型可见工具：注册表全量减去禁用前缀。
    pub fn visible_tool_specs(&self) -> Vec<crate::tools::ToolSpec> {
        self.registry
            .read()
            .map(|registry| {
                registry
                    .specs()
                    .into_iter()
                    .filter(|spec| !self.tool_disabled(&spec.name))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 设置共享窗口元素注册表（与 HTTP 感知层共用同一 ID 空间）。
    pub fn set_elements(&mut self, elements: Arc<Mutex<crate::ElementRegistry>>) {
        self.elements = elements;
    }

    pub fn elements(&self) -> Arc<Mutex<crate::ElementRegistry>> {
        Arc::clone(&self.elements)
    }

    pub fn skills(&self) -> &SkillRegistry {
        &self.skills
    }

    /// 当前 Agent 配置（只读快照，供诊断/上下文仪表展示）。
    pub fn config(&self) -> AgentConfig {
        self.config.clone()
    }

    pub fn provider(&self) -> Arc<dyn ModelProvider> {
        Arc::clone(&self.provider)
    }

    /// 直呼子代理（CLI `@explore` / `@subagent`）：独立子会话执行，返回最终文本。
    pub async fn run_subagent(
        &self,
        workspace: &std::path::Path,
        model: &str,
        prompt: &str,
        read_only: bool,
    ) -> Result<String, AgentError> {
        let abort = AtomicBool::new(false);
        // 直呼子代理没有可回传到客户端的审批通道：只读模式可以自动放行，
        // 通用模式必须默认拒绝写入/执行，避免子代理绕过主会话审批。
        let approver = crate::permissions::AutoApprover { allow: read_only };
        let runner = SubagentRunner {
            provider: Arc::clone(&self.provider),
            approver: &approver,
            abort: &abort,
            depth: self.config.subagent_depth,
            max_turns: self.config.max_turns,
            model: model.to_string(),
        };
        runner
            .run(workspace, prompt, read_only)
            .await
            .map_err(AgentError::Tool)
    }

    pub fn audit_log(&self) -> Arc<Mutex<AuditLog>> {
        Arc::clone(&self.audit)
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// 运行时追加危险命令片段（热生效；重启后由 settings.deny_commands 恢复）。
    pub fn add_runtime_deny(&self, fragment: impl Into<String>) {
        self.policy.add_runtime_deny(fragment);
    }

    /// 应用运行时权限设置（设置页保存后立即影响下一次工具调用）。
    pub fn apply_policy_settings(&self, read_only: bool, deny_commands: &[String]) {
        self.policy.set_read_only_runtime(read_only);
        self.policy.replace_runtime_deny(deny_commands);
    }

    pub fn registry(&self) -> Arc<RwLock<ToolRegistry>> {
        Arc::clone(&self.registry)
    }

    /// 执行一轮任务。审批经 `approver` 独立决策；`abort` 可随时中止。
    pub async fn run_turn(
        &self,
        session: &mut Session,
        prompt: &str,
        approver: &dyn Approver,
        abort: &AtomicBool,
        on_event: &mut (dyn FnMut(&TurnEvent) + Send),
    ) -> Result<TurnOutcome, AgentError> {
        let started_at = Utc::now().to_rfc3339();
        let started = std::time::Instant::now();
        let usage_before = self.provider.usage_snapshot();
        // 新回合代表从当前历史继续发展，旧的 rewind/undo 分支不能再恢复。
        session.redo_stack.clear();
        session.message_redo_stack.clear();
        let rules = load_project_rules(&session.workspace);
        let mut system = build_system_prompt(session.system_prompt.as_deref(), &rules);
        if !self.skills.list_enabled().is_empty() {
            let mut catalog = vec!["可用技能（通过 use_skill 工具按名调用）：".to_string()];
            for skill in self.skills.list_enabled() {
                catalog.push(format!("- {}：{}", skill.name, skill.description));
            }
            system.push_str("\n\n");
            system.push_str(&catalog.join("\n"));
        }
        let mut messages = vec![ChatMessage::system(system)];
        messages.extend(session.messages.iter().cloned());
        messages.push(ChatMessage::user(prompt.to_string()));
        let tools = self.visible_tool_specs();

        let mut events = Vec::new();
        let mut final_text = None;
        let mut steps = 0usize;

        for _index in 0..self.config.max_turns {
            if abort.load(Ordering::Relaxed) {
                commit_turn_messages(session, &messages);
                return Err(AgentError::Aborted);
            }
            let compaction = self.maybe_compact(&mut messages, &session.id).await;
            let summary = match compaction {
                Ok(summary) => summary,
                Err(error) => {
                    commit_turn_messages(session, &messages);
                    return Err(error);
                }
            };
            if let Some(summary) = summary {
                emit(
                    &mut events,
                    on_event,
                    TurnEvent::Compaction {
                        summary: summary.clone(),
                    },
                );
            }
            if messages.len() > self.config.context_limit {
                compact_truncate(&mut messages, self.config.context_limit);
            }

            emit(&mut events, on_event, TurnEvent::ModelCall);
            let on_event_reborrow = &mut *on_event;
            let mut emit_delta = |delta: String| {
                emit(
                    &mut events,
                    on_event_reborrow,
                    TurnEvent::TokenDelta { delta },
                );
            };
            let output = tokio::select! {
                output = self.provider.complete_stream(&messages, &tools, &mut emit_delta) => {
                    output.map_err(AgentError::Gateway)
                }
                _ = wait_for_abort(abort) => Err(AgentError::Aborted),
            };
            let output = match output {
                Ok(output) => output,
                Err(error) => {
                    commit_turn_messages(session, &messages);
                    return Err(error);
                }
            };

            match output {
                ModelOutput::Text(text) => {
                    messages.push(ChatMessage::assistant_text(text.clone()));
                    final_text = Some(text.clone());
                    emit(&mut events, on_event, TurnEvent::Final { text });
                    break;
                }
                ModelOutput::ToolCalls(calls) => {
                    messages.push(ChatMessage::assistant_tool_calls(calls.clone()));
                    for call in calls {
                        if abort.load(Ordering::Relaxed) {
                            commit_turn_messages(session, &messages);
                            return Err(AgentError::Aborted);
                        }
                        let request = self.policy.evaluate(&call.name, &call.arguments);
                        let decision = match self.policy.decision(&request) {
                            Decision::Ask => {
                                // 独立审批模型先于打扰用户（Auto-review）。
                                let verdict = if let Some(reviewer) = &self.reviewer {
                                    let context = session
                                        .messages
                                        .last()
                                        .and_then(|message| message.content.clone());
                                    reviewer.review(&request, context.as_deref()).await
                                } else {
                                    ReviewVerdict::Unknown
                                };
                                match verdict {
                                    ReviewVerdict::Deny => {
                                        self.audit
                                            .lock()
                                            .map_err(|_| AgentError::Session("审计锁中毒".into()))?
                                            .record(
                                                &session.id,
                                                "auto_review",
                                                Some(call.name.clone()),
                                                Some(false),
                                                format!("独立审批模型拒绝：{}", request.reason),
                                            );
                                        Decision::Deny
                                    }
                                    ReviewVerdict::Allow => {
                                        self.audit
                                            .lock()
                                            .map_err(|_| AgentError::Session("审计锁中毒".into()))?
                                            .record(
                                                &session.id,
                                                "auto_review",
                                                Some(call.name.clone()),
                                                Some(true),
                                                "独立审批模型放行".to_string(),
                                            );
                                        Decision::Allow
                                    }
                                    ReviewVerdict::Unknown => {
                                        emit(
                                            &mut events,
                                            on_event,
                                            TurnEvent::PermissionRequest(request.clone()),
                                        );
                                        approver.decide(&request).await
                                    }
                                }
                            }
                            other => other,
                        };
                        let approved = decision == Decision::Allow;
                        self.audit
                            .lock()
                            .map_err(|_| AgentError::Session("审计锁中毒".into()))?
                            .record(
                                &session.id,
                                "permission",
                                Some(call.name.clone()),
                                Some(approved),
                                request.reason.clone(),
                            );

                        let result = if self.tool_disabled(&call.name) {
                            Err(format!("工具已被禁用（插件热卸载）：{}", call.name))
                        } else if approved {
                            let workspace = session.workspace.clone();
                            emit(
                                &mut events,
                                on_event,
                                TurnEvent::ToolStart {
                                    id: call.id.clone(),
                                    tool: call.name.clone(),
                                },
                            );
                            let subagent = SubagentRunner {
                                provider: Arc::clone(&self.provider),
                                approver,
                                abort,
                                depth: self.config.subagent_depth,
                                max_turns: self.config.max_turns,
                                model: session.model.clone(),
                            };
                            let mut ctx = ToolContext {
                                workspace: &workspace,
                                policy: &self.policy,
                                session,
                                audit: &self.audit,
                                subagent: Some(subagent),
                                skills: &self.skills,
                                elements: &self.elements,
                            };
                            let tool = self
                                .registry
                                .read()
                                .map_err(|_| AgentError::Session("工具注册表锁中毒".into()))?
                                .get(&call.name);
                            let outcome = match tool {
                                Some(tool) => tool.run(&mut ctx, call.arguments.clone()).await,
                                None => Err(format!("未知工具：{}", call.name)),
                            };
                            emit(
                                &mut events,
                                on_event,
                                TurnEvent::ToolResult {
                                    id: call.id.clone(),
                                    tool: call.name.clone(),
                                    ok: outcome.is_ok(),
                                    error: outcome.as_ref().err().cloned(),
                                },
                            );
                            outcome
                        } else {
                            Err(format!("permission denied: {}", request.reason))
                        };

                        let raw_content = match &result {
                            Ok(value) => value.to_string(),
                            Err(error) => format!("工具错误：{error}"),
                        };
                        let content = sanitize_tool_result(
                            &call.name,
                            &truncate_tool_result(&raw_content, MAX_TOOL_RESULT_CHARS),
                        );
                        messages.push(ChatMessage::tool(call.id.clone(), content.clone()));
                        self.audit
                            .lock()
                            .map_err(|_| AgentError::Session("审计锁中毒".into()))?
                            .record(
                                &session.id,
                                "tool_call",
                                Some(call.name.clone()),
                                None,
                                content.clone(),
                            );
                        steps += 1;
                    }
                }
            }
        }

        if final_text.is_none() {
            commit_turn_messages(session, &messages);
            return Err(AgentError::Gateway(format!(
                "达到最大回合数（{}），任务未正常结束",
                self.config.max_turns
            )));
        }
        commit_turn_messages(session, &messages);
        let usage = self.provider.usage_snapshot().saturating_sub(&usage_before);
        Ok(TurnOutcome {
            final_text,
            steps,
            events,
            prompt: prompt.to_string(),
            started_at,
            duration_ms: started.elapsed().as_millis() as u64,
            usage,
        })
    }

    /// 当估算 token 超过预算时，用模型把旧历史压缩为摘要（保留最近消息）。
    async fn maybe_compact(
        &self,
        messages: &mut Vec<ChatMessage>,
        session_id: &str,
    ) -> Result<Option<String>, AgentError> {
        if !self.config.compaction_enabled || estimate_tokens(messages) <= self.config.token_budget
        {
            return Ok(None);
        }
        let head_end = messages.len().saturating_sub(self.config.keep_recent);
        if head_end < 4 {
            return Ok(None);
        }
        let head = messages[1..head_end].to_vec();
        let prompt = format!(
            "请把以下 Agent 会话历史压缩成一份简洁的进展摘要（保留：已完成的动作、\
             未完成事项、关键决策、当前上下文；不要编造新信息）：\n\n{}",
            serde_json::to_string(&head).unwrap_or_else(|_| "[]".to_string())
        );
        let summary = match self
            .provider
            .complete(&[ChatMessage::user(prompt)], &[])
            .await
        {
            Ok(crate::gateway::ModelOutput::Text(text)) => text,
            Ok(_) | Err(_) => return Ok(None),
        };
        let mut compacted = vec![messages[0].clone()];
        compacted.push(ChatMessage::system(format!(
            "历史摘要（已压缩）：\n{summary}"
        )));
        compacted.extend(messages[head_end..].to_vec());
        *messages = compacted;
        self.audit
            .lock()
            .map_err(|_| AgentError::Session("审计锁中毒".into()))?
            .record(
                session_id,
                "compaction",
                None,
                None,
                format!("压缩 {} 条历史消息", head.len()),
            );
        Ok(Some(summary))
    }
}

fn commit_turn_messages(session: &mut Session, messages: &[ChatMessage]) {
    session.messages = messages.iter().skip(1).cloned().collect();
    session.updated_at = Utc::now().to_rfc3339();
}

async fn wait_for_abort(abort: &AtomicBool) {
    while !abort.load(Ordering::Relaxed) {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

fn truncate_tool_result(content: &str, max_chars: usize) -> String {
    if content.chars().count() <= max_chars {
        return content.to_string();
    }
    let mut truncated: String = content.chars().take(max_chars).collect();
    truncated.push_str("\n[工具输出已截断]");
    truncated
}

/// 粗略 token 估算：字符数 / 2 + 每条消息固定开销。
pub fn estimate_tokens(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .map(|message| {
            let chars = message
                .content
                .as_deref()
                .map(str::chars)
                .map(|chars| chars.count())
                .unwrap_or(0);
            chars / 2 + 4
        })
        .sum()
}

fn emit(
    events: &mut Vec<TurnEvent>,
    on_event: &mut (dyn FnMut(&TurnEvent) + Send),
    event: TurnEvent,
) {
    on_event(&event);
    events.push(event);
}

fn compact_truncate(messages: &mut Vec<ChatMessage>, limit: usize) {
    if messages.len() <= limit {
        return;
    }
    let keep = limit.saturating_sub(1);
    let mut tail_start = messages.len().saturating_sub(keep);
    if tail_start < messages.len() && messages[tail_start].role == "tool" {
        let mut group_start = tail_start;
        while group_start > 1 && messages[group_start - 1].role == "tool" {
            group_start -= 1;
        }
        if group_start > 1
            && messages[group_start - 1].role == "assistant"
            && messages[group_start - 1].tool_calls.is_some()
        {
            tail_start = group_start - 1;
        } else {
            while tail_start < messages.len() && messages[tail_start].role == "tool" {
                tail_start += 1;
            }
        }
    }
    let mut tail = messages[tail_start..].to_vec();
    let system = messages[0].clone();
    tail.insert(0, system);
    *messages = tail;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_tokens_counts_chars_and_overhead() {
        let messages = vec![
            ChatMessage::system("规则".to_string()),
            ChatMessage::user("你好，请帮我总结这段代码".to_string()),
            ChatMessage::assistant_text("好的。".to_string()),
        ];
        let total = estimate_tokens(&messages);
        // 每条约 +4 开销：3 条 → 12；正文 ≈ (2 + 12 + 3)/2。
        assert!(
            (15..=25).contains(&total),
            "估算 token {total} 应在合理区间"
        );
    }

    #[test]
    fn empty_messages_cost_zero() {
        assert_eq!(estimate_tokens(&[]), 0);
    }

    #[test]
    fn compact_truncate_keeps_system_and_recent_tail() {
        let mut messages = vec![ChatMessage::system("系统".to_string())];
        for index in 0..10 {
            messages.push(ChatMessage::user(format!("消息{index}")));
        }
        compact_truncate(&mut messages, 4);
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, "system");
        assert!(messages
            .iter()
            .any(|message| message.content.as_deref() == Some("消息9")));
        assert!(messages
            .iter()
            .any(|message| message.content.as_deref() == Some("消息7")));
    }

    #[test]
    fn compact_truncate_keeps_tool_call_and_results_together() {
        let mut messages = vec![
            ChatMessage::system("系统".to_string()),
            ChatMessage::user("旧请求".to_string()),
            ChatMessage::assistant_tool_calls(vec![crate::gateway::ToolCall {
                id: "call-1".to_string(),
                name: "read_file".to_string(),
                arguments: serde_json::json!({ "path": "a.txt" }),
            }]),
            ChatMessage::tool("call-1".to_string(), "结果".to_string()),
            ChatMessage::user("继续".to_string()),
            ChatMessage::assistant_text("好的".to_string()),
        ];

        compact_truncate(&mut messages, 4);

        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "assistant");
        assert!(messages[1].tool_calls.is_some());
        assert_eq!(messages[2].role, "tool");
        assert_eq!(messages[2].tool_call_id.as_deref(), Some("call-1"));
    }

    #[test]
    fn tool_result_is_bounded_without_splitting_unicode() {
        let result = truncate_tool_result(&"中".repeat(10), 3);
        assert!(result.starts_with("中中中"));
        assert!(result.contains("工具输出已截断"));
    }
}
