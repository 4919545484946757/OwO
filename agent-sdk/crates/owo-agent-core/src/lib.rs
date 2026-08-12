//! OwO Agent SDK 核心库（M1）：
//! Agent loop、工具注册表、权限审批、会话、审计、模型网关。

pub mod agent;
pub mod audit;
pub mod context;
pub mod error;
pub mod eval;
pub mod gateway;
pub mod learn;
pub mod mcp;
pub mod perception;
pub mod permissions;
pub mod platform;
pub mod plugin;
pub mod session;
pub mod settings;
pub mod share;
pub mod skill;
pub mod skill_pack;
pub mod sqlite_store;
pub mod subagent;
pub mod tools;
pub mod trace;
pub mod whitelist;

pub use agent::{estimate_tokens, Agent, AgentConfig, TurnEvent, TurnOutcome};
pub use audit::{AuditEntry, AuditLog};
pub use error::AgentError;
pub use eval::{builtin_suite, eval_suite_path, run_suite, EvalCase, EvalReport, EvalSuite};
pub use gateway::{
    ChatMessage, ModelOutput, ModelProvider, OpenAiCompatibleConfig, OpenAiCompatibleProvider,
    ToolCall,
};
pub use learn::{
    ActionGraph, ActionNode, ActionType, FlowSkillManifest, FlowSkillPackage, FlowSkillStore,
    LearnRecorder, LearnState, ProactiveEngine, ProactiveSuggestion, RecordedAction,
    SemanticAnchor, Sensitivity, SuggestionAction,
};
pub use mcp::{McpClient, McpServerConfig, McpTool};
pub use perception::{
    CaptureMeta, ContentRef, ForegroundApp, PerceptionEvent, PerceptionLayer, SituationSnapshot,
    SituationStore, TaskHypothesis, UiContext,
};
pub use permissions::{Approver, Decision, Level, PermissionRequest, Policy};
pub use platform::{capture_screen, clipboard_sequence, poll_foreground_app};
pub use plugin::{discover_plugins, PluginManifest};
pub use session::{JsonSessionStore, Session, SessionStore};
pub use settings::Settings;
pub use share::{export_html, export_markdown};
pub use skill::{Skill, SkillRegistry};
pub use skill_pack::{
    discover_builtin_packages, install_builtin_packages, validate_skill_package,
    BuiltinSkillManifest, SkillPackageInfo,
};
pub use sqlite_store::SqliteSessionStore;
pub use tools::{Tool, ToolContext, ToolRegistry, ToolSpec};
pub use trace::{list_traces, load_trace, save_trace, TraceRecord};
pub use whitelist::{AppTier, Whitelist, WhitelistEntry};
