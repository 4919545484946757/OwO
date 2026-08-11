//! OwO Agent SDK 核心库（M1）：
//! Agent loop、工具注册表、权限审批、会话、审计、模型网关。

pub mod agent;
pub mod audit;
pub mod context;
pub mod error;
pub mod eval;
pub mod gateway;
pub mod mcp;
pub mod permissions;
pub mod session;
pub mod settings;
pub mod share;
pub mod skill;
pub mod sqlite_store;
pub mod subagent;
pub mod tools;
pub mod trace;

pub use agent::{estimate_tokens, Agent, AgentConfig, TurnEvent, TurnOutcome};
pub use audit::{AuditEntry, AuditLog};
pub use error::AgentError;
pub use eval::{builtin_suite, eval_suite_path, run_suite, EvalCase, EvalReport, EvalSuite};
pub use gateway::{
    ChatMessage, ModelOutput, ModelProvider, OpenAiCompatibleConfig, OpenAiCompatibleProvider,
    ToolCall,
};
pub use mcp::{McpClient, McpServerConfig, McpTool};
pub use permissions::{Approver, Decision, Level, PermissionRequest, Policy};
pub use session::{JsonSessionStore, Session, SessionStore};
pub use settings::Settings;
pub use share::{export_html, export_markdown};
pub use skill::{Skill, SkillRegistry};
pub use sqlite_store::SqliteSessionStore;
pub use tools::{Tool, ToolContext, ToolRegistry, ToolSpec};
pub use trace::{list_traces, load_trace, save_trace, TraceRecord};
