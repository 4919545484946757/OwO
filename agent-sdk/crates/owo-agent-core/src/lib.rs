//! OwO Agent SDK 核心库（M1）：
//! Agent loop、工具注册表、权限审批、会话、审计、模型网关。

pub mod accessibility;
pub mod action_program;
pub mod agent;
pub mod assert;
pub mod audit;
pub mod automation;
pub mod autoreview;
pub mod cloud_exec;
pub mod computer_task;
pub mod computer_use;
pub mod context;
pub mod element_registry;
pub mod error;
pub mod eval;
pub mod executor;
pub mod gateway;
pub mod goal;
pub mod injection;
pub mod learn;
pub mod locate;
pub mod mcp;
pub mod memory;
pub mod notes;
pub mod observe;
pub mod ocr;
#[cfg(target_os = "windows")]
pub mod onnx_ocr;
pub mod paddle_ocr;
pub mod perception;
pub mod permissions;
pub mod plan;
pub mod platform;
pub mod plugin;
pub mod scene;
pub mod session;
pub mod settings;
pub mod share;
pub mod share_skill;
pub mod skill;
pub mod skill_health;
pub mod skill_pack;
pub mod sqlite_store;
pub mod stt;
pub mod subagent;
pub mod tools;
pub mod trace;
pub mod vision;
pub mod whitelist;
pub mod window_template;
pub mod workflow;

pub use accessibility::{foreground_ui_tree, ui_tree_for_hwnd, UiNode};
pub use agent::{estimate_tokens, Agent, AgentConfig, TurnEvent, TurnOutcome};
pub use audit::{AuditEntry, AuditLog};
pub use automation::{AutomationAction, AutomationStore, AutomationTask, Schedule};
pub use autoreview::{
    parse_verdict, AutoReviewChain, HeuristicReviewer, ModelReviewer, ReviewVerdict, Reviewer,
};
pub use computer_task::{sensitive_ui_hit, ComputerTask, ComputerTaskRegistry, TaskState};
pub use context::load_project_rules;
pub use element_registry::{
    fuse_sources, fuse_sources_with_vision, register_vision_grounding, ElementRegistry,
    SceneElement, VisionGrounding,
};
pub use error::AgentError;
pub use eval::{builtin_suite, eval_suite_path, run_suite, EvalCase, EvalReport, EvalSuite};
pub use executor::{execute_graph, ExecReport, ExecStep, UiActionSource, WindowsUiaSource};
pub use gateway::{
    budget_violation, parse_usage_value, ChatMessage, ModelOutput, ModelProvider,
    OpenAiCompatibleConfig, OpenAiCompatibleProvider, TokenUsage, ToolCall,
};
pub use injection::{sanitize_tool_result, InjectionGuard, InjectionHit, InjectionSeverity};
pub use learn::{
    generalize_to_graph, recorded_actions_from_sequence, ActionGraph, ActionNode, ActionType,
    FlowSkillManifest, FlowSkillPackage, FlowSkillStore, LearnPipeline, LearnRecorder, LearnState,
    ProactiveEngine, ProactiveSuggestion, RecordedAction, SemanticAnchor, Sensitivity,
    SuggestionAction,
};
pub use mcp::{McpClient, McpRegistry, McpServerConfig, McpTool};
pub use observe::{
    desktop_observation, map_sim_events_to_actions, observation_from_sim_event, sample_desktop,
    value_hash, DesktopSnapshot, MemoryStore, Observation,
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
pub use plugin::{discover_plugins, plugin_mcp_config, PluginManifest, PluginStateStore};
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
    bmp_to_png, capture_vision_bmp, capture_vision_png, capture_vision_png_region,
    cross_validate_box, describe_image, ground_element, ollama_models, parse_verification,
    parse_vision_box, parse_vision_box_with_confidence, verification_prompt, vision_only_allowed,
    VisionBox, VisionConfig,
};
pub use whitelist::{AppTier, Whitelist, WhitelistEntry};
pub use window_template::{
    build_template, build_template_from_ocr, detect_template, detect_template_ocr, load_template,
    save_template, WindowRoi, WindowTemplate,
};
