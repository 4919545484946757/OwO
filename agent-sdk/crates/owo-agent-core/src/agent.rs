use crate::audit::AuditLog;
use crate::context::{build_system_prompt, load_project_rules};
use crate::error::AgentError;
use crate::gateway::{ChatMessage, ModelOutput, ModelProvider};
use crate::permissions::{Approver, Decision, PermissionRequest, Policy};
use crate::session::Session;
use crate::skill::SkillRegistry;
use crate::subagent::SubagentRunner;
use crate::tools::{ToolContext, ToolRegistry};
use chrono::Utc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub max_turns: usize,
    pub context_limit: usize,
    pub subagent_depth: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: 25,
            context_limit: 200,
            subagent_depth: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub enum TurnEvent {
    ModelCall,
    TokenDelta {
        delta: String,
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

#[derive(Debug, Clone)]
pub struct TurnOutcome {
    pub final_text: Option<String>,
    pub steps: usize,
    pub events: Vec<TurnEvent>,
}

/// Agent 核心：执行循环 + 工具注册表 + 权限策略 + 审计。
pub struct Agent {
    provider: Arc<dyn ModelProvider>,
    registry: ToolRegistry,
    policy: Policy,
    audit: Arc<Mutex<AuditLog>>,
    config: AgentConfig,
    skills: SkillRegistry,
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
            registry,
            policy,
            audit: Arc::new(Mutex::new(AuditLog::default())),
            config,
            skills: SkillRegistry::default(),
        }
    }

    pub fn set_skills(&mut self, skills: SkillRegistry) {
        self.skills = skills;
    }

    pub fn skills(&self) -> &SkillRegistry {
        &self.skills
    }

    pub fn audit_log(&self) -> Arc<Mutex<AuditLog>> {
        Arc::clone(&self.audit)
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
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
        let rules = load_project_rules(&session.workspace);
        let mut system = build_system_prompt(session.system_prompt.as_deref(), &rules);
        if !self.skills.list().is_empty() {
            let mut catalog = vec!["可用技能（通过 use_skill 工具按名调用）：".to_string()];
            for skill in self.skills.list() {
                catalog.push(format!("- {}：{}", skill.name, skill.description));
            }
            system.push_str("\n\n");
            system.push_str(&catalog.join("\n"));
        }
        let mut messages = vec![ChatMessage::system(system)];
        messages.extend(session.messages.iter().cloned());
        messages.push(ChatMessage::user(prompt.to_string()));
        let tools = self.registry.specs();

        let mut events = Vec::new();
        let mut final_text = None;
        let mut steps = 0usize;

        for _index in 0..self.config.max_turns {
            if abort.load(Ordering::Relaxed) {
                return Err(AgentError::Aborted);
            }
            if messages.len() > self.config.context_limit {
                compact(&mut messages, self.config.context_limit);
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
            let output = self
                .provider
                .complete_stream(&messages, &tools, &mut emit_delta)
                .await
                .map_err(AgentError::Gateway)?;

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
                            return Err(AgentError::Aborted);
                        }
                        let request = self.policy.evaluate(&call.name, &call.arguments);
                        let decision = match self.policy.decision(&request) {
                            Decision::Ask => {
                                emit(
                                    &mut events,
                                    on_event,
                                    TurnEvent::PermissionRequest(request.clone()),
                                );
                                approver.decide(&request).await
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

                        let result = if approved {
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
                            };
                            let outcome = self
                                .registry
                                .execute(&call.name, &mut ctx, call.arguments.clone())
                                .await;
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

                        let content = match &result {
                            Ok(value) => value.to_string(),
                            Err(error) => format!("工具错误：{error}"),
                        };
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

        session.messages = messages.into_iter().skip(1).collect();
        session.updated_at = Utc::now().to_rfc3339();
        Ok(TurnOutcome {
            final_text,
            steps,
            events,
        })
    }
}

fn emit(
    events: &mut Vec<TurnEvent>,
    on_event: &mut (dyn FnMut(&TurnEvent) + Send),
    event: TurnEvent,
) {
    on_event(&event);
    events.push(event);
}

fn compact(messages: &mut Vec<ChatMessage>, limit: usize) {
    if messages.len() <= limit {
        return;
    }
    let keep = limit.saturating_sub(1);
    let system = messages.remove(0);
    let tail_start = messages.len().saturating_sub(keep);
    let mut tail = messages.split_off(tail_start);
    tail.insert(0, system);
    *messages = tail;
}
