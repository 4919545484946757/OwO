//! 子代理：主 Agent 派生的嵌套会话（explore 只读 / subagent 通用）。

use crate::agent::{Agent, AgentConfig, TurnEvent};
use crate::gateway::ModelProvider;
use crate::permissions::{Approver, Policy};
use crate::session::Session;
use crate::tools::ToolRegistry;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub const MAX_SUBAGENT_DEPTH: usize = 2;

pub struct SubagentRunner<'a> {
    pub provider: Arc<dyn ModelProvider>,
    pub approver: &'a dyn Approver,
    pub abort: &'a AtomicBool,
    pub depth: usize,
    pub max_turns: usize,
    pub model: String,
}

impl SubagentRunner<'_> {
    /// 在只读或完整模式下运行一个子会话，返回最终文本。
    pub async fn run(
        &self,
        workspace: &Path,
        prompt: &str,
        read_only: bool,
    ) -> Result<String, String> {
        if self.depth >= MAX_SUBAGENT_DEPTH {
            return Err(format!("子代理深度超限（最多 {MAX_SUBAGENT_DEPTH} 层）"));
        }
        let policy = if read_only {
            Policy::read_only(workspace.to_path_buf())
        } else {
            Policy::new(workspace.to_path_buf())
        };
        let registry = if read_only {
            ToolRegistry::read_only()
        } else {
            ToolRegistry::new()
        };
        let config = AgentConfig {
            max_turns: self.max_turns.min(12),
            subagent_depth: self.depth + 1,
            ..Default::default()
        };
        let agent = Agent::new(Arc::clone(&self.provider), registry, policy, config);
        let system_prompt = if read_only {
            "你是只读探索子代理：只能读取/搜索工作区文件，禁止写入或执行命令；\
             调查完成后用简洁中文汇报发现。"
        } else {
            "你是通用子代理：独立完成委派任务，工具调用仍需审批，完成后汇报结果。"
        };
        let mut session = Session::new(
            workspace,
            self.model.clone(),
            Some(system_prompt.to_string()),
        );
        let mut on_event = |_event: &TurnEvent| {};
        let outcome = agent
            .run_turn(
                &mut session,
                prompt,
                self.approver,
                self.abort,
                &mut on_event,
            )
            .await
            .map_err(|error| format!("子代理执行失败：{error}"))?;
        Ok(outcome
            .final_text
            .unwrap_or_else(|| format!("（子代理无最终文本，共 {} 步）", outcome.steps)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{ChatMessage, ModelOutput, ModelProvider};
    use crate::permissions::AutoApprover;
    use crate::tools::ToolSpec;
    use async_trait::async_trait;

    struct FixedProvider;

    #[async_trait]
    impl ModelProvider for FixedProvider {
        async fn complete(
            &self,
            _messages: &[ChatMessage],
            _tools: &[ToolSpec],
        ) -> Result<ModelOutput, String> {
            Ok(ModelOutput::Text("ok".to_string()))
        }
    }

    #[tokio::test]
    async fn depth_limit_blocks_nested_run() {
        let workspace = std::env::temp_dir();
        let runner = SubagentRunner {
            provider: Arc::new(FixedProvider),
            approver: &AutoApprover { allow: true },
            abort: &AtomicBool::new(false),
            depth: MAX_SUBAGENT_DEPTH,
            max_turns: 5,
            model: "mock".to_string(),
        };
        let result = runner.run(&workspace, "x", true).await;
        assert!(result.unwrap_err().contains("深度超限"));
    }
}
