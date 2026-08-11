//! OwO Agent SDK 核心库（M1）：
//! Agent loop、工具注册表、权限审批、会话、审计、模型网关。

pub mod agent;
pub mod audit;
pub mod context;
pub mod error;
pub mod gateway;
pub mod permissions;
pub mod session;
pub mod tools;

pub use agent::{Agent, AgentConfig, TurnEvent, TurnOutcome};
pub use audit::{AuditEntry, AuditLog};
pub use error::AgentError;
pub use gateway::{
    ChatMessage, ModelOutput, ModelProvider, OpenAiCompatibleConfig, OpenAiCompatibleProvider,
    ToolCall,
};
pub use permissions::{Approver, Decision, Level, PermissionRequest, Policy};
pub use session::{JsonSessionStore, Session, SessionStore};
pub use tools::{Tool, ToolContext, ToolRegistry, ToolSpec};
