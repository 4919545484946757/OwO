//! OwO Agent SDK 核心库（M1）：
//! Agent loop、工具注册表、权限审批、会话、审计、模型网关。

pub mod accessibility;
pub mod action_program;
pub mod agent;
pub mod assert;
pub mod audit;
pub mod audit_chain;
pub mod automation;
pub mod autoreview;
pub mod blackboard;
pub mod bus_store;
pub mod capability;
pub mod cas_store;
pub mod cloud_exec;
pub mod computer_task;
pub mod computer_use;
pub mod context;
pub mod credentials;
pub mod critic;
pub mod element_registry;
pub mod error;
pub mod eval;
pub mod executor;
pub mod experience_store;
pub mod fleet;
pub mod fleet_transport;
pub mod gateway;
pub mod goal;
pub mod injection;
pub mod learn;
pub mod lease;
pub mod locate;
pub mod mcp;
pub mod memory;
pub mod node_agent;
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
pub mod remote_step;
pub mod sandbox;
pub mod scene;
pub mod session;
pub mod settings;
pub mod share;
pub mod share_skill;
pub mod skill;
pub mod skill_health;
pub mod skill_pack;
pub mod sqlite_store;
pub mod storage_crypto;
pub mod stt;
pub mod subagent;
pub mod tools;
pub mod trace;
pub mod vision;
pub mod whitelist;
pub mod window_template;
pub mod worker_pool;
pub mod workflow;

pub use accessibility::{foreground_ui_tree, ui_tree_for_hwnd, UiNode};
pub use agent::{estimate_tokens, Agent, AgentConfig, TurnEvent, TurnOutcome};
pub use audit::{AuditEntry, AuditLog};
pub use audit_chain::{
    canonical, export_to_file, hex_encode, hmac_sha256, load_export, verify_export, verify_file,
    Anchor, AuditChain, AuditChainError, AuditCliCommand, AuditCliOutcome, AuditExport,
    AuditRecord, ChainedRecord,
};
pub use automation::{AutomationAction, AutomationStore, AutomationTask, Schedule};
pub use autoreview::{
    parse_verdict, AutoReviewChain, HeuristicReviewer, ModelReviewer, ReviewVerdict, Reviewer,
};
pub use blackboard::{
    Blackboard, BlackboardEntry, BlackboardError, BlackboardEvent, BlackboardOp, BlackboardSnapshot,
};
pub use bus_store::{is_critical, BusPersistPolicy, BusStore, StoredMessage};
pub use capability::{
    evaluate_capability_match, Arch, CapabilityCard, CapabilityMatch, CapabilityWorkerRegistry,
    EgressMode, Os, RegistrySnapshot, Resources, RouteDecision, RouteStats, TrustLevel,
    WorkerHealth, WorkerRequirement,
};
pub use cas_store::{CasRefsSnapshot, CasStore};
pub use cloud_exec::{
    backoff_delay, cloud_token_from_env, describe_diff, validate_batch, validate_commands,
    CloudProgress, CloudTask, CloudTaskQueue, CloudTaskResult, CloudTaskSpec, CloudTransport,
    CollectingSink, DiffKind, FileDiff, HttpTransport, LocalSimExecutor, MockRemoteTransport,
    NullSink, ProgressSink, RemoteStatus, TaskRecord, TaskState as CloudTaskState, UsageMetrics,
};
pub use computer_task::{sensitive_ui_hit, ComputerTask, ComputerTaskRegistry, TaskState};
pub use computer_use::{
    desktop_click, desktop_click_gated, desktop_key, desktop_key_gated, desktop_launch,
    desktop_launch_gated, desktop_scroll, desktop_scroll_gated, desktop_shortcut, desktop_type,
    desktop_type_gated, run_approved_task, run_approved_task_on, scan_ui_sensitive,
    sim_base_url_configured, task_gate_check, SimTaskSurface, TaskGoal, TaskReport, TaskSurface,
};
pub use context::load_project_rules;
pub use credentials::{
    scan_json_for_secrets, windows_credential_manager, ApiKeyRef, CredentialError,
    CredentialResolver, CredentialStore, MemoryCredentialStore, ProviderConfig, UnavailableStore,
};
pub use critic::{
    review_loop, ConsistencyReport, Critic, CriticConfig, CriticVerdict, ReadOnlyGate,
    ReviewOutcome, ReviewRound, SamplePair, ScriptedCritic,
};
pub use element_registry::{
    fuse_sources, fuse_sources_with_vision, register_vision_grounding, ElementRegistry,
    SceneElement, VisionGrounding,
};
pub use error::AgentError;
pub use eval::{builtin_suite, eval_suite_path, run_suite, EvalCase, EvalReport, EvalSuite};
pub use executor::{execute_graph, ExecReport, ExecStep, UiActionSource, WindowsUiaSource};
pub use experience_store::{
    load_aggregation_report, AggregationReport, Attribution, ExperienceEvent, ExperienceKind,
    ExperienceStore, Outcome, SkillInsight, AGGREGATION_REPORT_FILE,
};
pub use fleet::{
    arbitrate_wait_cycle, backoff_secs, dedupe_messages, detect_cycle, detect_wait_cycle, fan_out,
    fan_out_cfg, is_mergeable, message_dedup_key, new_correlation_id, AgentBus, AgentId, Budget,
    BusError, BusMessage, CorrelationId, FanOutConfig, FanOutOutcome, FanOutReport, FanOutStatus,
    Mailbox, MessageKind, OverflowPolicy, PushOutcome, RestartPolicy, RestartRule,
    SupervisionState, Supervisor, WaitEdge, WaitGraph, WaitResolution, WorkerEvent,
    WorkerEventKind,
};
/// 控制面 HTTP 传输（cloud_exec 已有 `HttpTransport`，此处以 FleetHttpTransport 区分）。
pub use fleet_transport::HttpTransport as FleetHttpTransport;
pub use fleet_transport::{
    FleetTransport, InMemoryTransport, TransportEvent, TransportEventKind, TransportStatus,
    TransportTask, TransportWorker,
};
pub use gateway::{
    budget_violation, parse_usage_value, ChatMessage, ModelOutput, ModelProvider,
    OpenAiCompatibleConfig, OpenAiCompatibleProvider, TokenUsage, ToolCall,
};
pub use goal::{
    Goal, GoalBudget, GoalRunState, GoalRunner, GoalStatus, RunnerConfig, Worker, WorkerRegistry,
};
pub use injection::{sanitize_tool_result, InjectionGuard, InjectionHit, InjectionSeverity};
pub use learn::{
    generalize_to_graph, recorded_actions_from_sequence, ActionGraph, ActionNode, ActionType,
    FlowSkillManifest, FlowSkillPackage, FlowSkillStore, LearnPipeline, LearnRecorder, LearnState,
    ProactiveEngine, ProactiveSuggestion, RecordedAction, SemanticAnchor, Sensitivity,
    SuggestionAction,
};
pub use lease::{Lease, LeaseConfig, LeaseError, LeaseManager};
pub use mcp::{McpClient, McpRegistry, McpServerConfig, McpTool};
pub use node_agent::{NodeAgent, NodeStatus};
pub use notes::{
    add_block, append_child, block_text, doc_title, doc_to_md, generate_mixed_doc, get_block,
    insert_child, load_doc, md_to_doc, move_block, new_doc, remove_block, sanitize_html, save_doc,
    search_notes, walk, Block, BlockId, BlockKind, CanvasBlockData, CanvasNote, CanvasRect,
    FtsNoteIndex, InMemoryNoteIndex, NoteDoc, NoteIndex, NoteIndexer, SearchHit,
};
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
pub use plan::{verify_output, Plan, StepSpec, StepStatus, VerificationSpec};
pub use platform::{capture_screen, clipboard_sequence, poll_foreground_app};
pub use plugin::{
    discover_plugins, plugin_mcp_config, scan_plugin_for_risks, verify_plugin_signature,
    MarketPluginEntry, MarketUpdateManifest, PluginInstallReport, PluginInstallState,
    PluginManager, PluginManifest, PluginReviewState, PluginSignature, PluginStateStore,
    PluginSubmission, VersionsJson,
};
pub use remote_step::{
    approval_request_event, approve_transport_task, submit_via_transport,
    submit_via_transport_with_timeout, ApprovalSpec, EvidenceItem, RemoteStep, RemoteStepEvent,
    RemoteStepKind, RemoteStepOutcome,
};
pub use sandbox::{
    available_isolation, evaluate_capability, inside_workspace, probe_platform_support,
    CapabilityEvaluation, FileScope, IsolationLevel, MockSandboxExecutor, NetworkPolicy,
    PlatformSupport, SandboxAuditEvent, SandboxAuditLog, SandboxCommand, SandboxError,
    SandboxExecutor, SandboxHandle, SandboxHealth, SandboxManager, SandboxPolicy, SandboxProcess,
    SandboxProcessStatus,
};
pub use scene::{
    elements_from_ocr_lines, elements_from_ui_nodes, elements_from_vision_groundings,
    merge_sources, text_hash, ElementRelation, EntityState, Evidence, EvidenceSource, GraphElement,
    SceneGraph, WindowState,
};
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
pub use worker_pool::{
    IsolationMode, PoolError, PoolWorker, WorkerBudget, WorkerId, WorkerPool, WorkerSpec,
    WorkerStatus,
};
pub use workflow::{
    compile_to_program, eval_expr, validate_definition, ActSpec, ActionBackend, Approval,
    AutoApprover, CheckpointRef, HumanApprover, LocateSpec, MockBackend, PermMode, PermissionClaim,
    SenseSpec, StepRecord as WorkflowStepRecord, TriggerKind, WorkflowDefinition, WorkflowEngine,
    WorkflowOutcome, WorkflowState, WorkflowStep, WorkflowTrigger,
};
