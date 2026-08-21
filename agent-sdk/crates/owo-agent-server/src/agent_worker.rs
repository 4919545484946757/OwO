//! Goal/Plan 真实 Agent Worker（Agent 1，R5 子任务 2）。
//!
//! 独立编译模块（不使用 crate::/super::）。实现 `Worker`（name="agent"）：
//! run() 解析 `{prompt 必填, read_only 默认 true, model 可选}`，调用
//! `Agent::run_subagent`；model 取 input.model → OWO_AGENT_MODEL → 缺省 gpt-4.1-mini；
//! 无 OPENAI_API_KEY 返回可读错误（走既有重试/replan 语义），不 panic。
//!
//! 接线：lib.rs 需 `pub mod agent_worker;`（已在 DEPENDENCIES-agent1.md 留言）。

use async_trait::async_trait;
use owo_agent_core::goal::Worker;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

/// 真实 Agent 子代理 worker（只读子代理优先；写操作由 Agent 内部策略拒绝）。
pub struct AgentWorker {
    pub agent: Arc<owo_agent_core::Agent>,
    pub workspace: PathBuf,
}

impl AgentWorker {
    pub fn new(agent: Arc<owo_agent_core::Agent>, workspace: PathBuf) -> Self {
        Self { agent, workspace }
    }

    /// 模型解析：input.model → OWO_AGENT_MODEL → 缺省。
    pub fn resolve_model(input: &Value) -> String {
        input
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                std::env::var("OWO_AGENT_MODEL")
                    .ok()
                    .filter(|v| !v.is_empty())
            })
            .unwrap_or_else(|| "gpt-4.1-mini".to_string())
    }

    /// 凭据检查：无 OPENAI_API_KEY → Err（可读，不 panic）。
    pub fn check_credentials() -> Result<(), String> {
        let missing = std::env::var("OPENAI_API_KEY")
            .map(|v| v.trim().is_empty())
            .unwrap_or(true);
        if missing {
            Err("缺少 OPENAI_API_KEY，agent worker 无法调用模型（请配置凭据后重试）".to_string())
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl Worker for AgentWorker {
    fn name(&self) -> &str {
        "agent"
    }

    async fn run(&self, input: &Value) -> Result<String, String> {
        let prompt = input
            .get("prompt")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| "agent 步骤缺少 prompt 参数".to_string())?;
        let read_only = input
            .get("read_only")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        Self::check_credentials()?;
        let model = Self::resolve_model(input);
        let output = self
            .agent
            .run_subagent(&self.workspace, &model, prompt, read_only)
            .await
            .map_err(|e| format!("agent 子代理执行失败：{e}"))?;
        Ok(output)
    }
}

/// agent worker 的输入校验（plan 创建时预校验，缺 prompt → 400）。
pub fn validate_agent_input(input: &Value) -> Result<(), String> {
    let has_prompt = input
        .get("prompt")
        .and_then(Value::as_str)
        .map(|p| !p.trim().is_empty())
        .unwrap_or(false);
    if has_prompt {
        Ok(())
    } else {
        Err("agent 步骤 input 必须包含非空 prompt".to_string())
    }
}
