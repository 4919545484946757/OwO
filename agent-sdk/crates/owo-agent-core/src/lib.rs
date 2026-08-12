//! OwO Agent SDK 核心库（M1）：
//! Agent loop、工具注册表、权限审批、会话、审计、模型网关。

pub mod accessibility;
pub mod agent;
pub mod audit;
pub mod automation;
pub mod computer_use;
pub mod context;
pub mod element_registry;
pub mod error;
pub mod eval;
pub mod executor;
pub mod gateway;
pub mod learn;
pub mod mcp;
pub mod observe;
pub mod ocr;
pub mod paddle_ocr;
pub mod perception;
pub mod permissions;
pub mod platform;
pub mod plugin;
pub mod session;
pub mod settings;
pub mod share;
pub mod share_skill;
pub mod skill;
pub mod skill_pack;
pub mod sqlite_store;
pub mod stt;
pub mod subagent;
pub mod tools;
pub mod trace;
pub mod vision;
pub mod whitelist;
pub mod window_template;

pub use accessibility::{foreground_ui_tree, ui_tree_for_hwnd, UiNode};
pub use agent::{estimate_tokens, Agent, AgentConfig, TurnEvent, TurnOutcome};
pub use audit::{AuditEntry, AuditLog};
pub use automation::{AutomationAction, AutomationStore, AutomationTask, Schedule};
pub use element_registry::{fuse_sources, ElementRegistry, SceneElement};
pub use error::AgentError;
pub use eval::{builtin_suite, eval_suite_path, run_suite, EvalCase, EvalReport, EvalSuite};
pub use executor::{execute_graph, ExecReport, ExecStep, UiActionSource, WindowsUiaSource};
pub use gateway::{
    ChatMessage, ModelOutput, ModelProvider, OpenAiCompatibleConfig, OpenAiCompatibleProvider,
    ToolCall,
};
pub use learn::{
    generalize_to_graph, ActionGraph, ActionNode, ActionType, FlowSkillManifest, FlowSkillPackage,
    FlowSkillStore, LearnPipeline, LearnRecorder, LearnState, ProactiveEngine, ProactiveSuggestion,
    RecordedAction, SemanticAnchor, Sensitivity, SuggestionAction,
};
pub use mcp::{McpClient, McpServerConfig, McpTool};
pub use observe::{
    map_sim_events_to_actions, observation_from_sim_event, value_hash, MemoryStore, Observation,
};
pub use ocr::{
    crop_scale_bmp, group_ocr_lines, ocr_bmp, ocr_bmp_detailed, ocr_bmp_region, ocr_engine_status,
    OcrBox, OcrEngineStatus, OcrLine, OcrSummary,
};
pub use paddle_ocr::{ocr_paddle, ocr_preferred, paddle_enabled, parse_paddle_jsonl};
pub use perception::{
    CaptureMeta, ContentRef, ForegroundApp, PerceptionEvent, PerceptionLayer, SituationSnapshot,
    SituationStore, TaskHypothesis, UiContext,
};
pub use permissions::{Approver, Decision, Level, PermissionRequest, Policy};
pub use platform::{capture_screen, clipboard_sequence, poll_foreground_app};
pub use plugin::{discover_plugins, PluginManifest};
pub use session::{JsonSessionStore, Session, SessionStore};
pub use settings::{EgressSettings, Settings};
pub use share::{export_html, export_markdown};
pub use share_skill::{export_flow_skill_package, import_flow_skill_package};
pub use skill::{Skill, SkillRegistry};
pub use skill_pack::{
    discover_builtin_packages, install_builtin_packages, validate_skill_package,
    BuiltinSkillManifest, SkillPackageInfo,
};
pub use sqlite_store::SqliteSessionStore;
pub use stt::{LocalStt, SttOutcome};
pub use tools::{Tool, ToolContext, ToolRegistry, ToolSpec};
pub use trace::{list_traces, load_trace, save_trace, TraceRecord};
pub use vision::{
    bmp_to_png, capture_vision_bmp, capture_vision_png, cross_validate_box, describe_image,
    ground_element, ollama_models, parse_verification, parse_vision_box, VisionConfig,
};
pub use whitelist::{AppTier, Whitelist, WhitelistEntry};
pub use window_template::{
    build_template, build_template_from_ocr, detect_template, detect_template_ocr, load_template,
    save_template, WindowRoi, WindowTemplate,
};
