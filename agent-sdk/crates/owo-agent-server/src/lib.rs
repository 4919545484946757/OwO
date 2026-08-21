#![recursion_limit = "1024"]

//! OwO Agent SDK HTTP 服务（M1 + v0.4）：session/turn/permission/diff/revert/abort + SSE，
//! 以及 v0.4 接口：context.snapshot / perception.subscribe / learn.* / skill.verify /
//! proactive.suggest / whitelist.manage。
//!
//! 第四轮核心模块 HTTP/UI 集成：notes_api / plugin_market_api / workflow_api / goal_api /
//! sse（云端任务进度 SSE 集线器）四个模块路由并入 build_router；cloud_task_submit 的
//! ProgressSink 接 sse::sink(task_id) 使 /cloud/tasks/{id}/events 收到真实进度。
//!
//! 第五轮（R5）：workflow_api/goal_api 扩展（真实执行后端 + 人审 + run SSE + Agent Worker，
//! 子模块 workflow_backend/agent_worker 在各自模块内声明）；plugin_market_api 扩展
//! （远端市场 refresh/install-remote）+ team_api（团队技能包共享）+ market_client；
//! eval_gate（eval 护栏）+ observability_api（/metrics 可观测性）；memory_graph_api
//! （记忆图谱）+ intent_api（统一命令入口）。
//!
//! 第七轮（R7）：本地 API 安全边界（X03）——bearer token 鉴权（auth_token.rs，token 文件
//! 用户级 ACL + /auth/token 公开引导）、CORS 显式 origin 白名单（webview + localhost）、
//! 全局/每会话/敏感端点双令牌桶限流（rate_limit.rs，429 + Retry-After + 审计）；SSE
//! 资源型路径（/…/events）因 EventSource 无法携带自定义头而豁免鉴权（只读遥测）。
//!
//! 第八轮（R8）：存储运维（backup.rs：/storage/backup|restore|export|clear，恢复前自动备份、
//! zip-slip 防护、清空二次确认 + 完整性校验）与服务端韧性（shutdown.rs：全局并发 turn 上限、
//! 优雅关闭 POST /server/shutdown + GET /server/status、CLI serve 强杀恢复 pid 文件）。

mod auth_token;
pub mod backup;
mod error_codes;
mod eval_gate;
mod event_stream;
mod fleet_api;
mod goal_api;
mod idempotency;
mod intent_api;
mod logging;
mod market_client;
mod memory_graph_api;
mod notes_api;
mod observability_api;
mod plugin_market_api;
mod rate_limit;
pub mod shutdown;
mod slo;
mod sse;
mod team_api;
mod usage;
mod workflow_api;

/// 协议约束：新模块（notes_api 等）一律写全限定名 `owo_agent_server::AppState`，
/// 以便测试以 `#[path = "../src/xxx.rs"] mod` 独立编译；此处建立 crate 自别名，
/// 使该路径在库内（含子模块）同样可解析。
extern crate self as owo_agent_server;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use owo_agent_core::automation::{AutomationAction, AutomationStore, AutomationTask, Schedule};
use owo_agent_core::learn::{
    ActionType, LearnPipeline, LearnState, ProactiveEngine, ProactiveSuggestion, RecordedAction,
    SemanticAnchor, Sensitivity, SuggestionAction,
};
use owo_agent_core::locate::{locate, AnchorQuery};
use owo_agent_core::perception::{SituationSnapshot, SituationStore};
use owo_agent_core::permissions::{Approver, Decision, PermissionRequest};
use owo_agent_core::scene::{Evidence, EvidenceSource, GraphElement};
use owo_agent_core::session::{Session, SessionStore};
use owo_agent_core::validate_skill_package;
use owo_agent_core::whitelist::{Whitelist, WhitelistEntry};
use owo_agent_core::Agent;
use owo_agent_core::SceneElement;
use owo_agent_protocol::{
    CreateSessionRequest, EvalRunRequest, FileDiff, ForkRequest, HealthResponse,
    PermissionResponse, RewindRequest, SessionInfo, SseEvent, TurnRequest,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::wrappers::{ReceiverStream, UnboundedReceiverStream};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::services::ServeDir;

pub struct AppState {
    pub agent: Arc<Agent>,
    pub store: Arc<dyn SessionStore>,
    pub sessions: Arc<Mutex<HashMap<String, Session>>>,
    pub pending_approvals: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<Decision>>>>,
    pub pending_approval_sessions: Arc<Mutex<HashMap<String, String>>>,
    pub aborts: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    /// 每个会话一个运行锁，避免并发回合覆盖消息、快照和审计状态。
    pub turn_locks: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    pub traces_dir: PathBuf,
    pub perception: Arc<Mutex<SituationStore>>,
    pub whitelist: Arc<Mutex<Whitelist>>,
    pub pipeline: Arc<Mutex<LearnPipeline>>,
    pub proactive: Arc<Mutex<ProactiveEngine>>,
    pub stt: Arc<Mutex<owo_agent_core::LocalStt>>,
    pub automations: Arc<Mutex<AutomationStore>>,
    pub memory: Arc<Mutex<owo_agent_core::MemoryStore>>,
    pub audit_flushed: Arc<Mutex<usize>>,
    pub workspace: PathBuf,
    pub data_root: PathBuf,
    pub elements: Arc<Mutex<owo_agent_core::ElementRegistry>>,
    /// 插件启用状态（进程级热卸载的持久化基础）。
    pub plugin_state: Arc<Mutex<owo_agent_core::plugin::PluginStateStore>>,
    /// 持久场景图（跨请求保持模板命中率/历史命中先验；元素每请求从注册表刷新）。
    pub scene: Arc<Mutex<owo_agent_core::scene::SceneGraph>>,
    /// computer-use 任务注册表（任务级审批 + 熔断，m4d 前奏）。
    pub computer_tasks: Arc<owo_agent_core::ComputerTaskRegistry>,
    /// 云端执行队列（/cloud/* 路由；懒初始化，传输由环境变量决定）。
    /// tokio Mutex：异步 handler 内跨 await 持锁（std MutexGuard 非 Send）。
    pub cloud_queue: Arc<tokio::sync::Mutex<Option<owo_agent_core::cloud_exec::CloudTaskQueue>>>,
    /// 本地 API bearer token（X03：启动生成/复用 + 用户级 ACL 文件）。
    pub auth_token: Arc<auth_token::AuthToken>,
    /// 全局/每会话/敏感端点 双令牌桶限流（X03）。
    pub rate_limiter: Arc<rate_limit::RateLimiter>,
    /// R8 服务端韧性：全局并发 turn 上限 + 优雅关闭信号（CLI serve 接线退出）。
    pub shutdown_gate: Arc<shutdown::ShutdownGate>,
}

impl AppState {
    pub fn new(
        agent: Agent,
        store: impl SessionStore + 'static,
        traces_dir: PathBuf,
        data_root: PathBuf,
        workspace: PathBuf,
    ) -> Self {
        let settings = owo_agent_core::Settings::load(&workspace);
        settings.apply_usage_env();
        // R8：用量预算接线（Agent 4 交付 usage）——单价/预算从环境变量注入，turn 入口硬熔断。
        {
            let usage_store = usage::global();
            if let Some(price) = std::env::var("OWO_MODEL_INPUT_PRICE_PER_MTOK")
                .ok()
                .and_then(|value| value.parse::<f64>().ok())
            {
                let output_price = std::env::var("OWO_MODEL_OUTPUT_PRICE_PER_MTOK")
                    .ok()
                    .and_then(|value| value.parse::<f64>().ok())
                    .unwrap_or(price);
                usage_store.set_price_per_mtok(price.max(output_price));
            }
            if let Some(budget) = std::env::var("OWO_USAGE_COST_BUDGET_USD")
                .ok()
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|value| *value > 0.0)
            {
                usage_store.set_budget(usage::UsageDimension::Session, budget);
            }
        }
        let mut whitelist = Whitelist::default();
        for entry in settings.whitelist.clone() {
            whitelist.upsert(entry);
        }
        let elements = Arc::new(Mutex::new(owo_agent_core::ElementRegistry::new()));
        let mut agent = agent;
        agent.set_elements(elements.clone());
        // X03：本地 API bearer token（启动生成/复用 + ACL；失败降级为内存 token）。
        let auth_token = Arc::new(auth_token::AuthToken::load_or_create(&data_root));
        let rate_limiter = Arc::new(rate_limit::RateLimiter::from_env());
        let shutdown_gate = Arc::new(shutdown::ShutdownGate::from_env());
        Self {
            agent: Arc::new(agent),
            store: Arc::new(store),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            pending_approvals: Arc::new(Mutex::new(HashMap::new())),
            pending_approval_sessions: Arc::new(Mutex::new(HashMap::new())),
            aborts: Arc::new(Mutex::new(HashMap::new())),
            turn_locks: Arc::new(Mutex::new(HashMap::new())),
            traces_dir,
            perception: Arc::new(Mutex::new(SituationStore::new())),
            whitelist: Arc::new(Mutex::new(whitelist)),
            pipeline: Arc::new(Mutex::new(LearnPipeline::new(
                data_root.join("skills").join("user"),
            ))),
            proactive: Arc::new(Mutex::new(ProactiveEngine::new(settings.proactive.clone()))),
            stt: Arc::new(Mutex::new(owo_agent_core::LocalStt::new(
                &settings.stt,
                &data_root,
            ))),
            automations: Arc::new(Mutex::new(AutomationStore::new(data_root.clone()))),
            memory: Arc::new(Mutex::new(owo_agent_core::MemoryStore::new(
                data_root.join("memory.jsonl"),
            ))),
            audit_flushed: Arc::new(Mutex::new(0)),
            workspace,
            plugin_state: Arc::new(Mutex::new(owo_agent_core::plugin::PluginStateStore::new(
                Some(data_root.join("plugin_state.json")),
            ))),
            scene: Arc::new(Mutex::new(owo_agent_core::scene::SceneGraph::new())),
            computer_tasks: Arc::new(owo_agent_core::ComputerTaskRegistry::new()),
            cloud_queue: Arc::new(tokio::sync::Mutex::new(None)),
            data_root,
            elements,
            auth_token,
            rate_limiter,
            shutdown_gate,
        }
    }
}

pub fn build_router(state: Arc<AppState>) -> Router {
    // R7：SSE→可观测性指标桥接（Agent 4 钩子）：/events/stream 的采样样本
    // 转发到 observability_api（/metrics/runtime 呈现真实运行期数值）；
    // SLO 报告探针注册（/metrics/slo 反映全局 SLO 状态）。幂等：重复调用仅替换。
    event_stream::set_metrics_observer(Box::new(|sample| {
        observability_api::ingest_metrics_sample(&sample.to_json());
    }));
    observability_api::register_slo_report_probe(std::sync::Arc::new(slo::report_global));
    // R12（Agent 4 交付，主控接线）：用量/SLO 告警/SLO 周期报告探针注册，
    // 使 /metrics/prometheus 用量指标、/metrics/slo/alerts、/metrics/slo/report 返回真实数据
    // （此前仅注册 slo_report_probe，其余探针为未注册空 stub）。
    observability_api::register_usage_probe(std::sync::Arc::new(|| usage::global().summary()));
    observability_api::register_slo_alerts_probe(std::sync::Arc::new(|| slo::alerts_json(50)));
    observability_api::register_slo_period_probe(std::sync::Arc::new(slo::report_period_global));
    // R9 主控接线收尾：SLO 告警监听器转发到可靠事件流（/events/stream 收到 alert 事件）。
    // 触发源为 `slo::check_alerts_global`（数据面）；未评估时不产生事件，无副作用。
    slo::set_alert_listener(Box::new(|event| {
        let trace_id = event.trace_id.clone();
        let data = serde_json::to_string(event).unwrap_or_default();
        event_stream::hub().publish_alert(data, trace_id);
    }));
    // 公开面：健康检查 / OpenAPI / token 引导（静态桌面工作台 fallback 挂最终合并面）。
    let public = Router::new()
        .route("/health", get(health))
        .route("/openapi.json", get(openapi_spec))
        .route("/auth/token", get(auth_token::auth_token_bootstrap))
        .with_state(state.clone());
    // 保护面：全部业务 API（bearer token 鉴权 + 双令牌桶限流）。
    let protected = Router::new()
        .route("/usage", get(usage_summary))
        .route("/audit", get(audit_list))
        .route("/session", post(create_session))
        .route("/session/{id}", get(get_session))
        .route("/session/{id}/turn", post(turn))
        .route("/session/{id}/attachments", get(attachments_list))
        .route("/session/{id}/attachments", post(attachment_upload))
        .route(
            "/session/{id}/permission/{request_id}",
            post(respond_permission),
        )
        .route("/session/{id}/abort", post(abort_turn))
        .route("/session/{id}/diff", get(diff))
        .route("/session/{id}/revert", post(revert))
        .route("/session/{id}/fork", post(fork_session))
        .route("/session/{id}/rewind", post(rewind_session))
        .route("/session/{id}/redo", post(redo_session))
        .route("/session/{id}/rename", post(session_rename))
        .route("/session/{id}/archive", post(session_archive))
        .route("/session/{id}/pin", post(session_pin))
        .route("/session/{id}/children", get(children))
        .route("/session/{id}/export/{format}", get(export_session))
        .route("/sessions", get(list_sessions))
        .route("/skills", get(list_skills))
        .route("/skills/{name}", get(skill_detail).post(skill_edit))
        .route("/skills/{name}/enabled", post(skill_enabled))
        .route("/eval/run", post(run_eval))
        .route("/context/snapshot", get(context_snapshot))
        .route("/perception/events", get(perception_events))
        .route("/perception/capture", post(perception_capture))
        .route("/perception/layers", post(perception_layers))
        .route("/perception/tree", post(perception_tree))
        .route(
            "/perception/template/build",
            post(perception_template_build),
        )
        .route(
            "/perception/template/build-ocr",
            post(perception_template_build_ocr),
        )
        .route(
            "/perception/template/detect",
            post(perception_template_detect),
        )
        .route(
            "/perception/template/detect-ocr",
            post(perception_template_detect_ocr),
        )
        .route("/perception/elements", post(perception_elements))
        .route(
            "/perception/template/{app_id}",
            get(perception_template_get),
        )
        .route("/perception/ocr", post(perception_ocr))
        .route("/perception/ocr/bytes", post(perception_ocr_bytes))
        .route("/perception/ocr/status", get(ocr_status))
        .route("/perception/ocr/region", post(perception_ocr_region))
        .route("/perception/window", post(perception_window))
        .route("/desktop/foreground", get(desktop_foreground))
        .route("/desktop/windows", get(desktop_windows))
        .route("/desktop/activate", post(desktop_activate))
        .route("/desktop/click", post(desktop_click))
        .route("/desktop/type", post(desktop_type))
        .route("/desktop/key", post(desktop_key))
        .route("/desktop/shortcut", post(desktop_shortcut))
        .route("/desktop/launch", post(desktop_launch))
        .route("/desktop/scroll", post(desktop_scroll))
        .route("/desktop/wait", post(desktop_wait))
        .route("/vision/status", get(vision_status))
        .route("/vision/describe", post(vision_describe))
        .route("/vision/verify", post(vision_verify))
        .route("/vision/ground", post(vision_ground))
        .route("/memory/observations", get(memory_observations))
        .route("/memory/clear", post(memory_clear))
        .route("/memory/mine-skill", post(memory_mine_skill))
        .route("/learn/start", post(learn_start))
        .route("/learn/record", post(learn_record))
        .route("/learn/pause", post(learn_pause))
        .route("/learn/resume", post(learn_resume))
        .route("/learn/stop", post(learn_stop))
        .route("/learn/clear", post(learn_clear))
        .route("/learn/status", get(learn_status))
        .route("/learn/execute", post(learn_execute))
        .route("/learn/packages", get(learn_packages))
        .route(
            "/learn/packages/{name}",
            get(learn_package_detail).delete(learn_package_delete),
        )
        .route("/learn/sink", post(learn_sink))
        .route("/learn/execute-package", post(learn_execute_package))
        .route("/learn/export/{name}", get(learn_export))
        .route("/learn/import", post(learn_import))
        .route("/skill/verify", post(skill_verify))
        .route("/proactive/observe", post(proactive_observe))
        .route("/proactive/decide", post(proactive_decide))
        .route("/proactive/suggestions", get(proactive_suggestions))
        .route("/stt/transcribe", post(stt_transcribe))
        .route("/automations", get(automations_list))
        .route("/automations", post(automations_create))
        .route("/automations/{id}/toggle", post(automations_toggle))
        .route(
            "/automations/{id}",
            axum::routing::delete(automations_delete),
        )
        .route("/automations/reminders", get(automations_reminders))
        .route(
            "/automations/reminders/clear",
            post(automations_clear_reminders),
        )
        .route("/settings", get(settings_get).post(settings_update))
        .route("/settings/egress", post(settings_egress))
        .route("/whitelist", get(whitelist_list))
        .route("/whitelist/manage", post(whitelist_manage))
        .route("/session/{id}/context", get(session_context))
        .route("/skills/health", get(skills_health))
        .route("/skills/health/{name}/reset", post(skill_health_reset))
        .route("/plugins", get(plugins_list))
        .route("/plugins/{id}/enabled", post(plugin_enabled))
        .route("/subagent/run", post(subagent_run))
        .route(
            "/project/rules",
            get(project_rules_get).post(project_rules_post),
        )
        .route("/project/rules/template", post(project_rules_template))
        .route("/mcp", get(mcp_list))
        .route("/mcp/add", post(mcp_add))
        .route("/mcp/remove", post(mcp_remove))
        .route("/locate/query", post(locate_query))
        .route("/traces", get(traces_list))
        .route("/traces/{index}", get(trace_show))
        .route("/memory/recall", get(memory_recall))
        .route("/computer-use/tasks", get(computer_tasks_list))
        .route("/computer-use/task", post(computer_task_create))
        .route(
            "/computer-use/task/{id}/{action}",
            post(computer_task_transition),
        )
        .route(
            "/computer-use/task/{id}/check/{action}",
            get(computer_task_check),
        )
        .route(
            "/computer-use/sensitive-check",
            post(computer_sensitive_check),
        )
        .route("/computer-use/task/{id}/run", post(computer_task_run))
        .route("/cloud/tasks", post(cloud_task_submit))
        .route("/cloud/tasks/{id}", get(cloud_task_status))
        .route("/cloud/tasks/{id}/result", get(cloud_task_result))
        .route("/cloud/tasks/{id}/cancel", post(cloud_task_cancel))
        // R8 服务端韧性（并发上限/状态/优雅关闭）。
        .route("/server/status", get(server_status))
        .route("/server/shutdown", post(server_shutdown))
        // R8 用量预算：加额恢复（硬熔断后 request_topup 解除停轮）。
        .route("/usage/topup", post(usage_topup))
        // 与 R6 同款对齐：先 with_state 定 S，再 merge 模块 router（Router<()> 经 From 转换）。
        .with_state(state.clone())
        .merge(notes_api::router(state.clone()))
        .merge(plugin_market_api::router(state.clone()))
        .merge(workflow_api::router(state.clone()))
        .merge(goal_api::router(state.clone()))
        .merge(sse::router(state.clone()))
        // R5 第五轮：eval 护栏 / 团队共享 / 可观测性 / 记忆图谱 / 统一命令入口。
        // Agent 1 的审批（/workflow/run/{run_id}/approval）与 run SSE
        // （/workflow/run/{run_id}/events）已自含在 workflow_api::router 内，无需新 merge。
        .merge(team_api::router(state.clone()))
        .merge(eval_gate::router(state.clone()))
        .merge(observability_api::router(state.clone()))
        .merge(memory_graph_api::router(state.clone()))
        .merge(intent_api::router(state.clone()))
        // R8 存储运维（备份/恢复/导出/清空）。
        .merge(backup::router(state.clone()))
        // R8 用量与成本归集（Agent 4 交付：usage_router 四维用量 + 预算硬熔断）。
        .merge(usage::usage_router(state.clone()))
        // R10 契约治理：JSON Schema 版本化发布（/schemas/*）+ 契约变更 RFC 登记见本文件契约区。
        .route("/schemas", get(schemas_list))
        .route("/schemas/{kind}/{version}", get(schema_get))
        // R12（Agent 2 交付，主控挂载）：P2 双节点网格控制面 /fleet/*（节点注册/列表、
        // 任务提交/查询/取消/SSE 事件、审批响应；模块内 FleetHub 单例，不占用 AppState）。
        .merge(fleet_api::router(state.clone()))
        // R6（Wave 1，Agent 4 交付）：可靠事件流 /events/stream（SSE 续传 + 背压）。
        .merge(event_stream::router(state.clone()))
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        // 鉴权在最外层：未授权请求不进入限流，也不消耗令牌。
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth_token::require_auth,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            rate_limit::enforce_rate_limit,
        ));
    // 公开面（含静态 fallback）与保护面合并：两者均为 Router<Arc<AppState>>。
    // R8/R9：trace_id 贯穿置于最外层（public + protected + fallback 全覆盖）。
    public
        .merge(protected)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            trace_id_middleware,
        ))
        // R10：弃用策略——命中 DEPRECATED_ROUTES 附加 Deprecation 头。
        .layer(axum::middleware::from_fn(deprecation_middleware))
        .fallback_service(ServeDir::new(desktop_web_dir()))
        .layer(cors_layer())
}

/// R8/R9：trace_id 请求贯穿——从 `X-Trace-Id` 头继承（不合法则生成），回填响应头，
/// 设置全局 trace 上下文（Agent 4 logging：后台任务/SSE/指标可继承），
/// 并落一条结构化访问日志（脱敏不落消息体）。
async fn trace_id_middleware(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let inherited = request
        .headers()
        .get("x-trace-id")
        .and_then(|value| value.to_str().ok());
    let trace_id = logging::TraceId::from_header(inherited);
    logging::set_current_trace_id(Some(trace_id.as_str()));
    let method = request.method().to_string();
    let path = request.uri().path().to_string();
    let started = std::time::Instant::now();
    let mut response = next.run(request).await;
    if let Ok(value) = trace_id
        .to_header_value()
        .parse::<axum::http::HeaderValue>()
    {
        response.headers_mut().insert("x-trace-id", value);
    }
    logging::emit(
        logging::Level::Info,
        "http",
        Some(trace_id.as_str()),
        "request",
        &[
            ("method", serde_json::json!(method)),
            ("path", serde_json::json!(path)),
            ("status", serde_json::json!(response.status().as_u16())),
            (
                "duration_ms",
                serde_json::json!(started.elapsed().as_millis() as u64),
            ),
        ],
    );
    logging::set_current_trace_id(None);
    response
}

/// CORS：permissive → 显式 origin 白名单（webview 协议 + localhost/127.0.0.1 任意端口）。
/// 跨源预检由浏览器强制；服务器侧仍以 bearer token 鉴权为准。
fn cors_layer() -> CorsLayer {
    use axum::http::Method;
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _parts| {
            origin_allowed(origin.as_bytes())
        }))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
            axum::http::header::ACCEPT,
        ])
        .max_age(std::time::Duration::from_secs(600))
}

/// origin 白名单判定：localhost/127.0.0.1 任意端口、Tauri webview
/// （tauri://localhost、http(s)://tauri.localhost）。
fn origin_allowed(origin: &[u8]) -> bool {
    let Ok(origin) = std::str::from_utf8(origin) else {
        return false;
    };
    let host_port = origin.split("://").nth(1).unwrap_or(origin);
    let host = host_port.split(':').next().unwrap_or(host_port);
    host == "localhost" || host == "127.0.0.1" || host == "tauri.localhost"
}

/// 开发环境下的桌面工作台静态目录：`<repo>/agent-sdk/desktop/web`。
fn desktop_web_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|parent| parent.parent())
        .map(|root| root.join("desktop").join("web"))
        .unwrap_or_else(|| PathBuf::from("desktop/web"))
}

async fn openapi_spec() -> Json<Value> {
    Json(serde_json::json!({
        "openapi": "3.1.0",
        "info": { "title": "OwO Agent SDK API", "version": env!("CARGO_PKG_VERSION") },
        // R10 契约治理：API 版本号（破坏性变更递增 minor；弃用期 ≥2 minor）。
        "x-owo-api-version": OWO_API_VERSION,
        "servers": [{ "url": "http://127.0.0.1:4096" }],
        "paths": {
            "/health": { "get": { "operationId": "health", "responses": { "200": { "description": "ok" } } } },
            "/usage": { "get": { "operationId": "usageSummary", "responses": { "200": { "description": "model token usage snapshot and budget config" } } } },
            "/audit": { "get": { "operationId": "auditList", "parameters": [{ "name": "limit", "in": "query", "required": false, "schema": { "type": "integer" } }], "responses": { "200": { "description": "recent audit entries" } } } },
            "/session": { "post": {
                "operationId": "createSession",
                "requestBody": { "content": { "application/json": { "schema": { "$ref": "#/components/schemas/CreateSessionRequest" } } } },
                "responses": { "200": { "description": "session created", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SessionInfo" } } } } }
            } },
            "/session/{id}": { "get": { "operationId": "getSession", "parameters": [path_param("id")], "responses": { "200": { "description": "session detail with messages" } } } },
            "/session/{id}/turn": { "post": {
                "operationId": "agentTurn",
                "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }],
                "requestBody": { "content": { "application/json": { "schema": { "$ref": "#/components/schemas/TurnRequest" } } } },
                "responses": { "200": { "description": "SSE event stream" } }
            } },
            "/session/{id}/attachments": { "get": { "operationId": "attachmentsList", "parameters": [path_param("id")], "responses": { "200": { "description": "attachment list" } } }, "post": { "operationId": "attachmentUpload", "parameters": [path_param("id")], "responses": { "200": { "description": "uploaded attachment" } } } },
            "/session/{id}/abort": { "post": { "operationId": "abortTurn", "parameters": [path_param("id")], "responses": { "200": { "description": "ok" } } } },
            "/session/{id}/permission/{request_id}": { "post": { "operationId": "respondPermission", "parameters": [path_param("id"), path_param("request_id")], "responses": { "200": { "description": "ok" } } } },
            "/session/{id}/diff": { "get": { "operationId": "sessionDiff", "parameters": [path_param("id")], "responses": { "200": { "description": "diff list" } } } },
            "/session/{id}/revert": { "post": { "operationId": "sessionRevert", "parameters": [path_param("id")], "responses": { "200": { "description": "ok" } } } },
            "/session/{id}/fork": { "post": { "operationId": "sessionFork", "parameters": [path_param("id")], "responses": { "200": { "description": "forked session" } } } },
            "/session/{id}/rewind": { "post": { "operationId": "sessionRewind", "parameters": [path_param("id")], "responses": { "200": { "description": "ok" } } } },
            "/session/{id}/redo": { "post": { "operationId": "sessionRedo", "parameters": [path_param("id")], "responses": { "200": { "description": "ok" } } } },
            "/session/{id}/rename": { "post": { "operationId": "sessionRename", "parameters": [path_param("id")], "responses": { "200": { "description": "renamed session" } } } },
            "/session/{id}/archive": { "post": { "operationId": "sessionArchive", "parameters": [path_param("id")], "responses": { "200": { "description": "archive state" } } } },
            "/session/{id}/pin": { "post": { "operationId": "sessionPin", "parameters": [path_param("id")], "responses": { "200": { "description": "pin state" } } } },
            "/session/{id}/children": { "get": { "operationId": "sessionChildren", "parameters": [path_param("id")], "responses": { "200": { "description": "children" } } } },
            "/session/{id}/export/{format}": { "get": { "operationId": "exportSession", "parameters": [path_param("id"), path_param("format")], "responses": { "200": { "description": "md or html" } } } },
            "/sessions": { "get": { "operationId": "listSessions", "responses": { "200": { "description": "session list" } } } },
            "/skills": { "get": { "operationId": "listSkills", "responses": { "200": { "description": "skill list" } } } },
            "/skills/{name}": { "get": { "operationId": "skillDetail", "parameters": [path_param("name")], "responses": { "200": { "description": "skill detail with SKILL.md content" } } }, "post": { "operationId": "skillEdit", "parameters": [path_param("name")], "responses": { "200": { "description": "updated" } } } },
            "/skills/{name}/enabled": { "post": { "operationId": "skillEnabled", "parameters": [path_param("name")], "responses": { "200": { "description": "enabled state" } } } },
            "/eval/run": { "post": { "operationId": "runEval", "requestBody": { "content": { "application/json": { "schema": { "$ref": "#/components/schemas/EvalRunRequest" } } } }, "responses": { "200": { "description": "eval report" } } } },
            "/context/snapshot": { "get": { "operationId": "contextSnapshot", "responses": { "200": { "description": "situation snapshot" } } } },
            "/perception/events": { "get": { "operationId": "perceptionSubscribe", "responses": { "200": { "description": "SSE perception event stream" } } } },
            "/perception/capture": { "post": { "operationId": "perceptionCapture", "responses": { "200": { "description": "capture meta with OCR summary" } } } },
            "/perception/layers": { "post": { "operationId": "perceptionLayers", "responses": { "200": { "description": "layer authorization updated" } } } },
            "/perception/tree": { "post": { "operationId": "perceptionTree", "responses": { "200": { "description": "deep UI tree dump" } } } },
            "/perception/ocr": { "post": { "operationId": "perceptionOcr", "responses": { "200": { "description": "OCR text with bounding boxes" } } } },
            "/perception/ocr/status": { "get": { "operationId": "ocrStatus", "responses": { "200": { "description": "OCR engine diagnostics" } } } },
            "/perception/ocr/region": { "post": { "operationId": "perceptionOcrRegion", "responses": { "200": { "description": "region OCR text with bounding boxes" } } } },
            "/learn/record": { "post": { "operationId": "learnRecord", "responses": { "200": { "description": "learn state" } } } },
            "/learn/start": { "post": { "operationId": "learnStart", "responses": { "200": { "description": "learn state" } } } },
            "/learn/pause": { "post": { "operationId": "learnPause", "responses": { "200": { "description": "learn state" } } } },
            "/learn/resume": { "post": { "operationId": "learnResume", "responses": { "200": { "description": "learn state" } } } },
            "/learn/stop": { "post": { "operationId": "learnStop", "responses": { "200": { "description": "stopped with sample count" } } } },
            "/learn/clear": { "post": { "operationId": "learnClear", "responses": { "200": { "description": "ok" } } } },
            "/learn/execute": { "post": { "operationId": "learnExecute", "responses": { "200": { "description": "execution report" } } } },
            "/learn/packages": { "get": { "operationId": "learnPackages", "responses": { "200": { "description": "flow skill packages" } } } },
            "/learn/packages/{name}": { "get": { "operationId": "learnPackageDetail", "parameters": [path_param("name")], "responses": { "200": { "description": "package detail" } } }, "delete": { "operationId": "learnPackageDelete", "parameters": [path_param("name")], "responses": { "200": { "description": "deleted" } } } },
            "/learn/sink": { "post": { "operationId": "learnSink", "responses": { "200": { "description": "sunk package" } } } },
            "/learn/execute-package": { "post": { "operationId": "learnExecutePackage", "responses": { "200": { "description": "execution report" } } } },
            "/learn/export/{name}": { "get": { "operationId": "learnExport", "parameters": [path_param("name")], "responses": { "200": { "description": "owskill zip" } } } },
            "/learn/import": { "post": { "operationId": "learnImport", "responses": { "200": { "description": "imported package" } } } },
            "/skill/verify": { "post": { "operationId": "skillVerify", "responses": { "200": { "description": "validation result" } } } },
            "/proactive/observe": { "post": { "operationId": "proactiveObserve", "responses": { "200": { "description": "optional suggestion" } } } },
            "/proactive/decide": { "post": { "operationId": "proactiveDecide", "responses": { "200": { "description": "ok" } } } },
            "/proactive/suggestions": { "get": { "operationId": "proactiveSuggestions", "responses": { "200": { "description": "suggestion list" } } } },
            "/stt/transcribe": { "post": { "operationId": "sttTranscribe", "responses": { "200": { "description": "transcription text" } } } },
            "/automations": { "get": { "operationId": "automationsList", "responses": { "200": { "description": "automation tasks" } } }, "post": { "operationId": "automationsCreate", "responses": { "200": { "description": "created task" } } } },
            "/automations/{id}/toggle": { "post": { "operationId": "automationsToggle", "parameters": [path_param("id")], "responses": { "200": { "description": "enabled state" } } } },
            "/automations/{id}": { "delete": { "operationId": "automationsDelete", "parameters": [path_param("id")], "responses": { "200": { "description": "ok" } } } },
            "/automations/reminders": { "get": { "operationId": "automationsReminders", "responses": { "200": { "description": "pending reminders" } } } },
            "/automations/reminders/clear": { "post": { "operationId": "automationsClearReminders", "responses": { "200": { "description": "ok" } } } },
            "/settings": { "get": { "operationId": "settingsGet", "responses": { "200": { "description": "workspace settings" } } }, "post": { "operationId": "settingsUpdate", "responses": { "200": { "description": "workspace settings" } } } },
            "/settings/egress": { "post": { "operationId": "settingsEgress", "responses": { "200": { "description": "cloud enabled state" } } } },
            "/whitelist": { "get": { "operationId": "whitelistList", "responses": { "200": { "description": "whitelist entries" } } } },
            "/session/{id}/context": { "get": { "operationId": "sessionContext", "parameters": [path_param("id")], "responses": { "200": { "description": "context stats: messages/tokens/budget/compaction/rules" } } } },
            "/skills/health": { "get": { "operationId": "skillsHealth", "responses": { "200": { "description": "flow skill health overview" } } } },
            "/skills/health/{name}/reset": { "post": { "operationId": "skillHealthReset", "parameters": [path_param("name")], "responses": { "200": { "description": "health reset" } } } },
            "/plugins": { "get": { "operationId": "pluginsList", "responses": { "200": { "description": "discovered plugins with manifests" } } } },
            "/plugins/{id}/enabled": { "post": { "operationId": "pluginEnabled", "parameters": [path_param("id")], "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "enabled": { "type": "boolean" } }, "required": ["enabled"] } } } }, "responses": { "200": { "description": "plugin enabled state" } } } },
            "/subagent/run": { "post": { "operationId": "subagentRun", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "prompt": { "type": "string" }, "read_only": { "type": "boolean" }, "model": { "type": "string" } }, "required": ["prompt"] } } } }, "responses": { "200": { "description": "subagent execution result" } } } },
            "/project/rules": { "get": { "operationId": "projectRulesGet", "responses": { "200": { "description": "AGENTS.md/CLAUDE.md rules with injection status" } } }, "post": { "operationId": "projectRulesPost", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "content": { "type": "string" } }, "required": ["content"] } } } }, "responses": { "200": { "description": "rules written" } } } },
            "/project/rules/template": { "post": { "operationId": "projectRulesTemplate", "responses": { "200": { "description": "AGENTS.md template written" } } } },
            "/mcp": { "get": { "operationId": "mcpList", "responses": { "200": { "description": "configured MCP servers" } } } },
            "/mcp/add": { "post": { "operationId": "mcpAdd", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "name": { "type": "string" }, "transport": { "type": "string", "enum": ["stdio", "http"] }, "command": { "type": "string" }, "args": { "type": "array", "items": { "type": "string" } }, "url": { "type": "string" } }, "required": ["name", "transport"] } } } }, "responses": { "200": { "description": "server added and connected" } } } },
            "/mcp/remove": { "post": { "operationId": "mcpRemove", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "name": { "type": "string" } }, "required": ["name"] } } } }, "responses": { "200": { "description": "server removed" } } } },
            "/locate/query": { "post": { "operationId": "locateQuery", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "app_id": { "type": "string" }, "role": { "type": "string" }, "name_pattern": { "type": "string" }, "parent": { "type": "string" }, "stable_id": { "type": "string" }, "min_confidence": { "type": "number" } }, "required": [] } } } }, "responses": { "200": { "description": "multi-source locate result" } } } },
            "/traces": { "get": { "operationId": "tracesList", "responses": { "200": { "description": "trace list" } } } },
            "/traces/{index}": { "get": { "operationId": "traceShow", "parameters": [path_param("index")], "responses": { "200": { "description": "trace detail" } } } },
            "/memory/observations": { "get": { "operationId": "memoryObservations", "responses": { "200": { "description": "situation memory observations" } } } },
            "/memory/recall": { "get": { "operationId": "memoryRecall", "responses": { "200": { "description": "semantic memory recall" } } } },
            "/memory/clear": { "post": { "operationId": "memoryClear", "responses": { "200": { "description": "memory cleared" } } } },
            "/memory/mine-skill": { "post": { "operationId": "memoryMineSkill", "responses": { "200": { "description": "mined flow skill package" } } } },
            "/whitelist/manage": { "post": { "operationId": "whitelistManage", "responses": { "200": { "description": "whitelist entries" } } } },
            "/computer-use/tasks": { "get": { "operationId": "computerTasksList", "responses": { "200": { "description": "computer-use task list" } } } },
            "/computer-use/task": { "post": { "operationId": "computerTaskCreate", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "target_app": { "type": "string" }, "description": { "type": "string" }, "allowed_actions": { "type": "array", "items": { "type": "string" } }, "max_duration_ms": { "type": "integer" } }, "required": ["target_app"] } } } }, "responses": { "200": { "description": "task created (Pending)" } } } },
            "/computer-use/task/{id}/{action}": { "post": { "operationId": "computerTaskTransition", "parameters": [path_param("id"), path_param("action")], "responses": { "200": { "description": "task state transitioned" } } } },
            "/computer-use/task/{id}/check/{action}": { "get": { "operationId": "computerTaskCheck", "parameters": [path_param("id"), path_param("action")], "responses": { "200": { "description": "task executable check" } } } },
            "/computer-use/sensitive-check": { "post": { "operationId": "computerSensitiveCheck", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "name": { "type": "string" }, "role": { "type": "string" }, "ocr_text": { "type": "string" } }, "required": ["name"] } } } }, "responses": { "200": { "description": "sensitive ui detection" } } } },
            "/computer-use/task/{id}/run": { "post": { "operationId": "computerTaskRun", "parameters": [path_param("id")], "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "goals": { "type": "array", "items": { "type": "object", "properties": { "anchor_text": { "type": "string" }, "action": { "type": "string" }, "value": { "type": "string" }, "verify_text": { "type": "string" } } } } } } } } }, "responses": { "200": { "description": "approved task executed (closed loop)" } } } },
            "/cloud/tasks": { "post": { "operationId": "cloudTaskSubmit", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "name": { "type": "string" }, "workspace_dir": { "type": "string" }, "commands": { "type": "array", "items": { "type": "string" } }, "env_passthrough": { "type": "array", "items": { "type": "string" } }, "timeout_secs": { "type": "integer" } } } } } }, "responses": { "200": { "description": "cloud task submitted and executed" } } } },
            "/cloud/tasks/{id}": { "get": { "operationId": "cloudTaskStatus", "parameters": [path_param("id")], "responses": { "200": { "description": "cloud task status + usage" } } } },
            "/cloud/tasks/{id}/result": { "get": { "operationId": "cloudTaskResult", "parameters": [path_param("id")], "responses": { "200": { "description": "cloud task result + diff summary" } } } },
            "/cloud/tasks/{id}/cancel": { "post": { "operationId": "cloudTaskCancel", "parameters": [path_param("id")], "responses": { "200": { "description": "cloud task canceled" } } } },
            "/openapi.json": { "get": { "operationId": "openapiSpec", "responses": { "200": { "description": "OpenAPI 3.1 spec" } } } },
            "/perception/elements": { "post": { "operationId": "perceptionElements", "responses": { "200": { "description": "element registry snapshot" } } } },
            "/perception/ocr/bytes": { "post": { "operationId": "perceptionOcrBytes", "responses": { "200": { "description": "OCR text from raw image bytes" } } } },
            "/perception/window": { "post": { "operationId": "perceptionWindow", "responses": { "200": { "description": "active window info" } } } },
            "/perception/template/build": { "post": { "operationId": "perceptionTemplateBuild", "responses": { "200": { "description": "window template built" } } } },
            "/perception/template/build-ocr": { "post": { "operationId": "perceptionTemplateBuildOcr", "responses": { "200": { "description": "window template built with OCR" } } } },
            "/perception/template/detect": { "post": { "operationId": "perceptionTemplateDetect", "responses": { "200": { "description": "template detection result" } } } },
            "/perception/template/detect-ocr": { "post": { "operationId": "perceptionTemplateDetectOcr", "responses": { "200": { "description": "template detection with OCR" } } } },
            "/perception/template/{app_id}": { "get": { "operationId": "perceptionTemplateGet", "parameters": [path_param("app_id")], "responses": { "200": { "description": "stored window template" } } } },
            "/learn/status": { "get": { "operationId": "learnStatus", "responses": { "200": { "description": "learn pipeline state" } } } },
            "/desktop/foreground": { "get": { "operationId": "desktopForeground", "responses": { "200": { "description": "foreground window info" } } } },
            "/desktop/windows": { "get": { "operationId": "desktopWindows", "responses": { "200": { "description": "window list" } } } },
            "/desktop/activate": { "post": { "operationId": "desktopActivate", "responses": { "200": { "description": "window activated" } } } },
            "/desktop/click": { "post": { "operationId": "desktopClick", "responses": { "200": { "description": "mouse click performed" } } } },
            "/desktop/type": { "post": { "operationId": "desktopType", "responses": { "200": { "description": "text typed" } } } },
            "/desktop/key": { "post": { "operationId": "desktopKey", "responses": { "200": { "description": "key pressed" } } } },
            "/desktop/shortcut": { "post": { "operationId": "desktopShortcut", "responses": { "200": { "description": "shortcut performed" } } } },
            "/desktop/launch": { "post": { "operationId": "desktopLaunch", "responses": { "200": { "description": "app launched" } } } },
            "/desktop/scroll": { "post": { "operationId": "desktopScroll", "responses": { "200": { "description": "scroll performed" } } } },
            "/desktop/wait": { "post": { "operationId": "desktopWait", "responses": { "200": { "description": "wait performed" } } } },
            "/vision/status": { "get": { "operationId": "visionStatus", "responses": { "200": { "description": "vision engine diagnostics" } } } },
            "/vision/describe": { "post": { "operationId": "visionDescribe", "responses": { "200": { "description": "image description" } } } },
            "/vision/verify": { "post": { "operationId": "visionVerify", "responses": { "200": { "description": "verification result" } } } },
            "/vision/ground": { "post": { "operationId": "visionGround", "responses": { "200": { "description": "vision grounded location" } } } },
            "/notes": { "get": { "operationId": "notesList", "responses": { "200": { "description": "note list" } } }, "post": { "operationId": "notesCreate", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "title": { "type": "string" }, "markdown": { "type": "string" } }, "required": ["title"] } } } }, "responses": { "201": { "description": "note created" } } } },
            "/notes/{id}": { "get": { "operationId": "notesGet", "parameters": [path_param("id")], "responses": { "200": { "description": "note block tree" } } }, "put": { "operationId": "notesReplace", "parameters": [path_param("id")], "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "title": { "type": "string" }, "blocks": { "type": "array", "items": { "type": "object" } } } } } } }, "responses": { "200": { "description": "note replaced" } } }, "delete": { "operationId": "notesDelete", "parameters": [path_param("id")], "responses": { "200": { "description": "note deleted" } } } },
            "/notes/import": { "post": { "operationId": "notesImport", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "title": { "type": "string" }, "markdown": { "type": "string" } }, "required": ["title", "markdown"] } } } }, "responses": { "201": { "description": "note imported from markdown" } } } },
            "/notes/search": { "get": { "operationId": "notesSearch", "parameters": [{ "name": "q", "in": "query", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "description": "cross-document search hits" } } } },
            "/notes/{id}/export/{format}": { "get": { "operationId": "notesExport", "parameters": [path_param("id"), path_param("format")], "responses": { "200": { "description": "note exported as md or html" } } } },
            "/notes/{id}/blocks": { "post": { "operationId": "notesAddBlock", "parameters": [path_param("id")], "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "parent": { "type": "string" }, "after": { "type": "string" }, "kind": { "type": "string" }, "text": { "type": "string" }, "data": { "type": "object" } }, "required": ["kind"] } } } }, "responses": { "201": { "description": "block added" } } } },
            "/notes/{id}/blocks/move": { "post": { "operationId": "notesMoveBlock", "parameters": [path_param("id")], "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "block_id": { "type": "string" }, "parent": { "type": "string" }, "after": { "type": "string" } }, "required": ["block_id"] } } } }, "responses": { "200": { "description": "block moved" } } } },
            "/notes/{id}/blocks/{block_id}": { "patch": { "operationId": "notesUpdateBlock", "parameters": [path_param("id"), path_param("block_id")], "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "text": { "type": "string" }, "data": { "type": "object" } } } } } }, "responses": { "200": { "description": "block updated" } } }, "delete": { "operationId": "notesDeleteBlock", "parameters": [path_param("id"), path_param("block_id")], "responses": { "200": { "description": "removed block subtree ids" } } } },
            "/notes/{id}/reindex": { "post": { "operationId": "notesReindex", "parameters": [path_param("id")], "responses": { "200": { "description": "full-text index rebuilt" } } } },
            "/workflow": { "get": { "operationId": "workflowList", "responses": { "200": { "description": "discovered .owflow flows" } } } },
            "/workflow/validate": { "post": { "operationId": "workflowValidate", "requestBody": { "content": { "application/json": { "schema": { "type": "object" } } } }, "responses": { "200": { "description": "definition validation report" } } } },
            "/workflow/{name}": { "get": { "operationId": "workflowGet", "parameters": [path_param("name")], "responses": { "200": { "description": "flow definition with validation" } } } },
            "/workflow/{name}/run": { "post": { "operationId": "workflowRun", "parameters": [path_param("name")], "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "ctx": { "type": "object" } } } } } }, "responses": { "201": { "description": "workflow run started" } } } },
            "/workflow/{name}/runs": { "get": { "operationId": "workflowRuns", "parameters": [path_param("name")], "responses": { "200": { "description": "run list for flow" } } } },
            "/workflow/run/{run_id}": { "get": { "operationId": "workflowRunSnapshot", "parameters": [path_param("run_id")], "responses": { "200": { "description": "run snapshot" } } } },
            "/workflow/run/{run_id}/abort": { "post": { "operationId": "workflowRunAbort", "parameters": [path_param("run_id")], "responses": { "200": { "description": "abort requested" } } } },
            "/workflow/run/{run_id}/audit": { "get": { "operationId": "workflowRunAudit", "parameters": [path_param("run_id")], "responses": { "200": { "description": "run audit tail" } } } },
            "/goal": { "get": { "operationId": "goalList", "responses": { "200": { "description": "goal list" } } }, "post": { "operationId": "goalCreate", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "objective": { "type": "string" }, "budget": { "type": "object", "properties": { "max_steps": { "type": "integer" }, "max_replans": { "type": "integer" } } } }, "required": ["objective"] } } } }, "responses": { "201": { "description": "goal created" } } } },
            "/goal/{id}": { "get": { "operationId": "goalGet", "parameters": [path_param("id")], "responses": { "200": { "description": "goal detail" } } } },
            "/goal/{id}/plan": { "get": { "operationId": "goalPlanGet", "parameters": [path_param("id")], "responses": { "200": { "description": "goal plan" } } }, "post": { "operationId": "goalPlanCreate", "parameters": [path_param("id")], "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "steps": { "type": "array", "items": { "type": "object" } } }, "required": ["steps"] } } } }, "responses": { "201": { "description": "plan created with waves preview" } } } },
            "/goal/{id}/run": { "post": { "operationId": "goalRun", "parameters": [path_param("id")], "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "config": { "type": "object", "properties": { "parallelism": { "type": "integer" }, "allow_replan": { "type": "boolean" } } } } } } } }, "responses": { "202": { "description": "run started" } } } },
            "/goal/{id}/status": { "get": { "operationId": "goalStatus", "parameters": [path_param("id")], "responses": { "200": { "description": "goal run state snapshot" } } } },
            "/goal/{id}/abort": { "post": { "operationId": "goalAbort", "parameters": [path_param("id")], "responses": { "200": { "description": "abort requested" } } } },
            "/goal/{id}/audit": { "get": { "operationId": "goalAudit", "parameters": [path_param("id")], "responses": { "200": { "description": "goal audit tail" } } } },
            "/goal/{id}/runs": { "get": { "operationId": "goalRuns", "parameters": [path_param("id")], "responses": { "200": { "description": "goal run list" } } } },
            "/cloud/tasks/{id}/events": { "get": { "operationId": "cloudTaskEvents", "parameters": [path_param("id")], "responses": { "200": { "description": "SSE progress stream for cloud task" } } } },
            "/plugins/market": { "get": { "operationId": "pluginMarketCatalog", "responses": { "200": { "description": "plugin market catalog merged with local" } } } },
            "/plugins/market/seed": { "post": { "operationId": "pluginMarketSeed", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "entries": { "type": "array", "items": { "type": "object" } } }, "required": ["entries"] } } } }, "responses": { "200": { "description": "market seeded" } } } },
            "/plugins/market/versions": { "get": { "operationId": "pluginMarketVersions", "parameters": [{ "name": "id", "in": "query", "required": true, "schema": { "type": "string" } }, { "name": "app", "in": "query", "required": false, "schema": { "type": "string" } }], "responses": { "200": { "description": "compatible version resolution" } } } },
            "/plugins/market/verify": { "post": { "operationId": "pluginMarketVerify", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "dir": { "type": "string" } }, "required": ["dir"] } } } }, "responses": { "200": { "description": "plugin dir verified" } } } },
            "/plugins/market/install": { "post": { "operationId": "pluginMarketInstall", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "dir": { "type": "string" } }, "required": ["dir"] } } } }, "responses": { "200": { "description": "plugin installed" } } } },
            "/plugins/market/update": { "post": { "operationId": "pluginMarketUpdate", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "id": { "type": "string" }, "dir": { "type": "string" } }, "required": ["id", "dir"] } } } }, "responses": { "200": { "description": "plugin updated" } } } },
            "/plugins/market/uninstall": { "post": { "operationId": "pluginMarketUninstall", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] } } } }, "responses": { "200": { "description": "plugin uninstalled" } } } },
            "/plugins/market/scan": { "get": { "operationId": "pluginMarketScan", "parameters": [{ "name": "dir", "in": "query", "required": false, "schema": { "type": "string" } }], "responses": { "200": { "description": "risk scan summary" } } } },
            "/plugins/market/audit": { "get": { "operationId": "pluginMarketAudit", "parameters": [{ "name": "n", "in": "query", "required": false, "schema": { "type": "integer" } }], "responses": { "200": { "description": "plugin market audit tail" } } } },
            "/plugins/market/refresh": { "post": { "operationId": "pluginMarketRefresh", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "url": { "type": "string" } } } } } }, "responses": { "200": { "description": "market registry refreshed" } } } },
            "/plugins/market/install-remote": { "post": { "operationId": "pluginMarketInstallRemote", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "id": { "type": "string" }, "version": { "type": "string" }, "url": { "type": "string" } }, "required": ["id"] } } } }, "responses": { "200": { "description": "remote plugin signed and installed" } } } },
            "/team/export": { "post": { "operationId": "teamExport", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "type": { "type": "string" }, "id": { "type": "string" } }, "required": ["type", "id"] } } } }, "responses": { "200": { "description": "packaged skill bytes + manifest summary" } } } },
            "/team/review": { "post": { "operationId": "teamReview", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "package_b64": { "type": "string" } }, "required": ["package_b64"] } } } }, "responses": { "200": { "description": "review findings without import" } } } },
            "/team/import": { "post": { "operationId": "teamImport", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "package_b64": { "type": "string" } }, "required": ["package_b64"] } } } }, "responses": { "200": { "description": "imported or blocked with findings" } } } },
            "/team/versions": { "get": { "operationId": "teamVersions", "parameters": [{ "name": "id", "in": "query", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "description": "team package version history" } } } },
            "/team/audit": { "get": { "operationId": "teamAudit", "responses": { "200": { "description": "team api audit tail" } } } },
            "/eval/gate/run": { "post": { "operationId": "evalGateRun", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "suite": { "type": "string" }, "model": { "type": "string" } } } } } }, "responses": { "200": { "description": "eval report or skipped reason" } } } },
            "/eval/gate/report": { "get": { "operationId": "evalGateReport", "responses": { "200": { "description": "latest eval report" } } } },
            "/eval/gate/reports": { "get": { "operationId": "evalGateReports", "responses": { "200": { "description": "eval report history" } } } },
            "/schemas": { "get": { "operationId": "schemasList", "responses": { "200": { "description": "JSON Schema 版本化发布索引（plugin-manifest/owskill/owflow）" } } } },
            "/schemas/{kind}/{version}": { "get": { "operationId": "schemaGet", "parameters": [path_param("kind"), path_param("version")], "responses": { "200": { "description": "JSON Schema (draft-07)" } } } },
            "/metrics/overview": { "get": { "operationId": "metricsOverview", "responses": { "200": { "description": "aggregated traces/tools/approvals metrics" } } } },
            "/metrics/turns": { "get": { "operationId": "metricsTurns", "parameters": [{ "name": "limit", "in": "query", "required": false, "schema": { "type": "integer" } }], "responses": { "200": { "description": "recent turn durations" } } } },
            "/metrics/tools": { "get": { "operationId": "metricsTools", "responses": { "200": { "description": "tool call frequency and failure ranking" } } } },
            "/metrics/health": { "get": { "operationId": "metricsHealth", "responses": { "200": { "description": "component health checklist" } } } },
            "/memory/graph/entries": { "get": { "operationId": "memoryGraphEntries", "parameters": [{ "name": "app", "in": "query", "required": false, "schema": { "type": "string" } }, { "name": "from", "in": "query", "required": false, "schema": { "type": "string" } }, { "name": "to", "in": "query", "required": false, "schema": { "type": "string" } }, { "name": "limit", "in": "query", "required": false, "schema": { "type": "integer" } }], "responses": { "200": { "description": "structured memory entries" } } } },
            "/memory/graph/timeline": { "get": { "operationId": "memoryGraphTimeline", "parameters": [{ "name": "from", "in": "query", "required": false, "schema": { "type": "string" } }, { "name": "to", "in": "query", "required": false, "schema": { "type": "string" } }], "responses": { "200": { "description": "time-bucketed timeline" } } } },
            "/memory/graph/entities": { "get": { "operationId": "memoryGraphEntities", "parameters": [{ "name": "limit", "in": "query", "required": false, "schema": { "type": "integer" } }], "responses": { "200": { "description": "entity/tag aggregation" } } } },
            "/memory/graph/links": { "get": { "operationId": "memoryGraphLinks", "responses": { "200": { "description": "manual relation list" } } } },
            "/memory/graph/link": { "post": { "operationId": "memoryGraphLinkAdd", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "a": { "type": "string" }, "b": { "type": "string" }, "relation": { "type": "string" }, "note": { "type": "string" } }, "required": ["a", "b", "relation"] } } } }, "responses": { "201": { "description": "relation added" } } }, "delete": { "operationId": "memoryGraphLinkDelete", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "a": { "type": "string" }, "b": { "type": "string" }, "relation": { "type": "string" } }, "required": ["a", "b", "relation"] } } } }, "responses": { "200": { "description": "relation removed" } } } },
            "/memory/graph/recall": { "get": { "operationId": "memoryGraphRecall", "parameters": [{ "name": "q", "in": "query", "required": true, "schema": { "type": "string" } }, { "name": "top_k", "in": "query", "required": false, "schema": { "type": "integer" } }], "responses": { "200": { "description": "recall with entity hits" } } } },
            "/intent/parse": { "post": { "operationId": "intentParse", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] } } } }, "responses": { "200": { "description": "parsed intent with args and confidence" } } } },
            "/command/run": { "post": { "operationId": "commandRun", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "mode": { "type": "string" }, "text": { "type": "string" }, "wav_b64": { "type": "string" } }, "required": ["mode"] } } } }, "responses": { "200": { "description": "intent routed to action with results" } } } },
            "/command/audit": { "get": { "operationId": "commandAudit", "responses": { "200": { "description": "command execution audit tail" } } } },
            "/workflow/run/{run_id}/approval": { "post": { "operationId": "workflowRunApproval", "parameters": [path_param("run_id")], "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "decision": { "type": "string" } }, "required": ["decision"] } } } }, "responses": { "200": { "description": "approval decision recorded" } } } },
            "/workflow/run/{run_id}/events": { "get": { "operationId": "workflowRunEvents", "parameters": [path_param("run_id")], "responses": { "200": { "description": "SSE run event stream" } } } },
            "/events/stream": { "get": { "operationId": "eventsStream", "parameters": [{ "name": "last_event_id", "in": "query", "required": false, "schema": { "type": "integer" } }], "responses": { "200": { "description": "reliable SSE event stream (Last-Event-ID resume + bounded backpressure)" } } } },
            "/metrics/runtime": { "get": { "operationId": "metricsRuntime", "responses": { "200": { "description": "runtime process metrics" } } } },
            "/metrics/slo": { "get": { "operationId": "metricsSlo", "responses": { "200": { "description": "SLO registry with error budget and attainment status" } } } },
            "/metrics/slo/alerts": { "get": { "operationId": "metricsSloAlerts", "responses": { "200": { "description": "SLO alert rules and structured alert events" } } } },
            "/metrics/slo/report": { "get": { "operationId": "metricsSloReport", "parameters": [{ "name": "days", "in": "query", "required": false, "schema": { "type": "integer" } }], "responses": { "200": { "description": "SLO period report (JSON)" } } } },
            "/metrics/prometheus": { "get": { "operationId": "metricsPrometheus", "responses": { "200": { "description": "Prometheus text exposition format" } } } },
            "/auth/token": { "get": { "operationId": "authTokenBootstrap", "security": [], "responses": { "200": { "description": "public bootstrap token (same-origin pairing; CORS whitelist blocks cross-origin reads)" } } } },
            "/storage/backup": { "post": { "operationId": "storageBackup", "responses": { "200": { "description": "zip backup (b64 + saved path)" } } } },
            "/storage/restore": { "post": { "operationId": "storageRestore", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "archive_b64": { "type": "string" } }, "required": ["archive_b64"] } } } }, "responses": { "200": { "description": "restore result with pre-backup" } } } },
            "/storage/export": { "post": { "operationId": "storageExport", "responses": { "200": { "description": "full standard JSON export" } } } },
            "/storage/clear": { "post": { "operationId": "storageClear", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "confirm": { "type": "string", "enum": ["CLEAR_ALL"] } } } } } }, "responses": { "200": { "description": "cleared with integrity check" } } } },
            "/server/status": { "get": { "operationId": "serverStatus", "responses": { "200": { "description": "concurrency gate + storage migration status" } } } },
            "/server/shutdown": { "post": { "operationId": "serverShutdown", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "confirm": { "type": "boolean" } }, "required": ["confirm"] } } } }, "responses": { "200": { "description": "graceful shutdown requested" } } } },
            "/usage/summary": { "get": { "operationId": "usageSummaryV2", "responses": { "200": { "description": "four-dimension usage aggregation + budget hard-stop state" } } } },
            "/usage/records": { "get": { "operationId": "usageRecords", "parameters": [{ "name": "dimension", "in": "query", "required": false, "schema": { "type": "string", "enum": ["session", "workflow_run", "goal_step", "tool"] } }, { "name": "limit", "in": "query", "required": false, "schema": { "type": "integer" } }], "responses": { "200": { "description": "usage records filtered by dimension" } } } },
            "/usage/report": { "get": { "operationId": "usageReport", "parameters": [{ "name": "days", "in": "query", "required": false, "schema": { "type": "integer" } }], "responses": { "200": { "description": "usage aggregation report over window (budget/soak friendly)" } } } },
            "/usage/topup": { "post": { "operationId": "usageTopup", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "amount": { "type": "number" } } } } } }, "responses": { "200": { "description": "budget topped up and hard stop cleared" } } } },
            "/fleet/nodes/register": { "post": { "operationId": "fleetNodesRegister", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "node_id": { "type": "string" }, "card": { "type": "object" } }, "required": ["node_id", "card"] } } } }, "responses": { "200": { "description": "node registered with lease" } } } },
            "/fleet/nodes": { "get": { "operationId": "fleetNodesList", "responses": { "200": { "description": "node status snapshots" } } } },
            "/fleet/tasks/submit": { "post": { "operationId": "fleetTasksSubmit", "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "task_id": { "type": "string" }, "worker": { "type": "string" }, "input": { "type": "object" }, "correlation_id": { "type": "string" }, "lineage": { "type": "array", "items": { "type": "string" } }, "approval_required": { "type": "boolean" } }, "required": ["task_id", "worker", "input"] } } } }, "responses": { "200": { "description": "task submitted with idempotency key" } } } },
            "/fleet/tasks/{id}": { "get": { "operationId": "fleetTaskGet", "parameters": [path_param("id")], "responses": { "200": { "description": "task view with status and events" } } } },
            "/fleet/tasks/{id}/cancel": { "post": { "operationId": "fleetTaskCancel", "parameters": [path_param("id")], "responses": { "200": { "description": "task cancelled" } } } },
            "/fleet/tasks/{id}/events": { "get": { "operationId": "fleetTaskEvents", "parameters": [path_param("id"), { "name": "format", "in": "query", "required": false, "schema": { "type": "string", "enum": ["json"] } }], "responses": { "200": { "description": "SSE task event stream (history replay + live; ?format=json returns array)" } } } },
            "/fleet/approvals/{id}/respond": { "post": { "operationId": "fleetApprovalRespond", "parameters": [path_param("id")], "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "decision": { "type": "string", "enum": ["approve", "reject"] }, "approved_by": { "type": "string" } }, "required": ["decision", "approved_by"] } } } }, "responses": { "200": { "description": "approval decision recorded" } } } }
        },
        "components": {
            "schemas": {
                "CreateSessionRequest": {
                    "type": "object",
                    "properties": {
                        "workspace": { "type": "string" },
                        "model": { "type": "string" },
                        "system_prompt": { "type": "string" }
                    },
                    "required": ["workspace"]
                },
                "SessionInfo": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "workspace": { "type": "string" },
                        "updated_at": { "type": "string" },
                        "title": { "type": "string" },
                        "archived": { "type": "boolean" },
                        "pinned": { "type": "boolean" },
                        "parent_id": { "type": "string" },
                        "fork_point": { "type": "integer" },
                        "model": { "type": "string" },
                        "created_at": { "type": "string" }
                    }
                },
                "TurnRequest": {
                    "type": "object",
                    "properties": {
                        "prompt": { "type": "string" },
                        "attachments": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["prompt"]
                },
                "EvalRunRequest": {
                    "type": "object",
                    "properties": { "suite_id": { "type": "string" } },
                    "required": ["suite_id"]
                }
            },
            "securitySchemes": {
                "bearerAuth": { "type": "http", "scheme": "bearer" }
            }
        },
        "security": [{ "bearerAuth": [] }]
    }))
}

fn path_param(name: &str) -> Value {
    serde_json::json!({ "name": name, "in": "path", "required": true, "schema": { "type": "string" } })
}

fn to_session_info(session: &Session) -> SessionInfo {
    SessionInfo {
        id: session.id.clone(),
        workspace: session.workspace.to_string_lossy().into_owned(),
        model: session.model.clone(),
        created_at: session.created_at.clone(),
        updated_at: session.updated_at.clone(),
        title: Some(session.display_title()),
        archived: session.archived,
        pinned: session.pinned,
        parent_id: session.parent_id.clone(),
        fork_point: session.fork_point,
    }
}

fn load_session(state: &AppState, id: &str) -> Result<Session, (StatusCode, String)> {
    if let Ok(sessions) = state.sessions.lock() {
        if let Some(session) = sessions.get(id) {
            return Ok(session.clone());
        }
    }
    state.store.load(id).map_err(|error| {
        (
            StatusCode::NOT_FOUND,
            format!("会话不存在：{id}（{error}）"),
        )
    })
}

async fn acquire_session_lock(
    state: &AppState,
    id: &str,
) -> Result<tokio::sync::OwnedMutexGuard<()>, (StatusCode, String)> {
    let lock = {
        let mut locks = state.turn_locks.lock().map_err(poison)?;
        locks
            .entry(id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    Ok(lock.lock_owned().await)
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        healthy: true,
        version: env!("CARGO_PKG_VERSION").to_string(),
        auto_approve: auto_approve_enabled(),
    })
}

/// 当前模型用量快照 + 预算配置（供桌面端“设置与诊断”用量面板展示）。
async fn usage_summary(State(state): State<Arc<AppState>>) -> Json<Value> {
    let usage = state.agent.provider().usage_snapshot();
    let input_price = std::env::var("OWO_MODEL_INPUT_PRICE_PER_MTOK")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.0);
    let output_price = std::env::var("OWO_MODEL_OUTPUT_PRICE_PER_MTOK")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.0);
    let token_cap = std::env::var("OWO_USAGE_TOKEN_BUDGET")
        .ok()
        .and_then(|value| value.parse::<u64>().ok());
    let cost_cap = std::env::var("OWO_USAGE_COST_BUDGET_USD")
        .ok()
        .and_then(|value| value.parse::<f64>().ok());
    let cost = usage.cost_estimate_usd(input_price, output_price);
    let violation =
        owo_agent_core::budget_violation(&usage, token_cap, cost_cap, input_price, output_price);
    Json(json!({
        "usage": usage,
        "cost_usd": cost,
        "budget": {
            "token_cap": token_cap,
            "cost_cap_usd": cost_cap,
            "input_price_per_mtok": input_price,
            "output_price_per_mtok": output_price,
            "violation": violation,
        },
    }))
}

/// 把 Agent 内存审计日志中尚未落库的条目追加到存储，返回已 flush 数。
pub fn flush_audit(state: &AppState) {
    let mut flushed = match state.audit_flushed.lock() {
        Ok(flushed) => flushed,
        Err(_) => return,
    };
    let log = state.agent.audit_log();
    let audit = match log.lock() {
        Ok(audit) => audit,
        Err(_) => return,
    };
    if audit.entries.len() > *flushed {
        let entries = audit.entries[*flushed..].to_vec();
        let next = audit.entries.len();
        drop(audit);
        if state.store.append_audit(&entries).is_ok() {
            *flushed = next;
        }
    }
}

/// R8：服务运行状态（并发上限/在途/关闭中 + 存储只读降级提示）。
async fn server_status(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({
        "shutdown_gate": {
            "max_concurrent_turns": state.shutdown_gate.max_concurrent(),
            "active_turns": state.shutdown_gate.active_turns(),
            "shutting_down": state.shutdown_gate.shutting_down(),
        },
        "storage": {
            "read_only": state.store.is_read_only(),
            "migration_warning": state.store.migration_warning(),
        },
    }))
}

#[derive(serde::Deserialize)]
struct ShutdownRequest {
    confirm: Option<bool>,
}

/// R8：优雅关闭入口（需二次确认；CLI serve 侧接线完成「停止接收→完成在途→flush→退出」）。
async fn server_shutdown(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ShutdownRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if request.confirm != Some(true) {
        return Err((
            StatusCode::BAD_REQUEST,
            "需要二次确认：{\"confirm\":true}".to_string(),
        ));
    }
    let active = state.shutdown_gate.request_shutdown();
    logging::warn(
        "server",
        None,
        "收到优雅关闭请求（需二次确认）",
        &[("active_turns", json!(active))],
    );
    Ok(Json(json!({
        "ok": true,
        "shutting_down": true,
        "active_turns": active,
        "note": "已停止接收新回合；在途回合完成后服务将退出（CLI serve 接线）",
    })))
}

/// R8：用量预算加额（解除硬熔断；主控接线 Agent 4 usage::request_topup）。
#[derive(serde::Deserialize)]
struct UsageTopupRequest {
    /// 加额（美元）；不填则仅解除熔断。
    amount: Option<f64>,
}

async fn usage_topup(
    State(state): State<Arc<AppState>>,
    Json(request): Json<UsageTopupRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let amount = request.amount.unwrap_or(0.0);
    if !amount.is_finite() || amount < 0.0 {
        // R10：错误码表统一响应体（validation/invalid_input → 400）。
        let code = error_codes::ErrorCode::from_code("validation/invalid_input/not_retryable")
            .unwrap_or_else(|_| error_codes::ErrorCode {
                domain: "validation".into(),
                reason: "invalid_input".into(),
                retryable: false,
                http_status: 400,
                retry_after_ms: None,
            });
        return Err(api_error_response(
            &code,
            format!("amount 非法：{amount}（需 ≥0）"),
        ));
    }
    usage::global().request_topup(usage::UsageDimension::Session, amount);
    logging::info(
        "usage",
        None,
        &format!("预算加额 ${amount:.2}，硬熔断已解除"),
    );
    Ok(Json(json!({
        "ok": true,
        "topup_usd": amount,
        "hard_stopped": usage::global().is_hard_stopped(),
        "note": "会话维度预算已加额，熔断解除（见 /usage/summary）",
        "workspace": state.workspace.to_string_lossy(),
    })))
}

async fn audit_list(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<owo_agent_core::AuditEntry>>, (StatusCode, String)> {
    flush_audit(&state);
    let limit = params
        .get("limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100)
        .min(500);
    let query = owo_agent_core::sqlite_store::AuditQuery {
        limit,
        offset: params
            .get("offset")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0),
        event: params
            .get("event")
            .filter(|value| !value.trim().is_empty())
            .cloned(),
        tool: params
            .get("tool")
            .filter(|value| !value.trim().is_empty())
            .cloned(),
        approved: params.get("approved").and_then(|value| value.parse().ok()),
        q: params
            .get("q")
            .filter(|value| !value.trim().is_empty())
            .cloned(),
    };
    let (entries, _) = state.store.query_audit(&query);
    Ok(Json(entries))
}

async fn list_sessions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<SessionInfo>>, (StatusCode, String)> {
    let mut sessions = Vec::new();
    for session_id in state.store.list() {
        if let Ok(session) = state.store.load(&session_id) {
            sessions.push(to_session_info(&session));
        }
    }
    sessions.sort_by(|left, right| {
        right
            .pinned
            .cmp(&left.pinned)
            .then_with(|| right.updated_at.cmp(&left.updated_at))
    });
    Ok(Json(sessions))
}

async fn list_skills(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<Value>>, (StatusCode, String)> {
    let registry = state.agent.skills();
    let skills = registry.list();
    Ok(Json(
        skills
            .iter()
            .map(|skill| {
                json!({
                    "name": skill.name,
                    "description": skill.description,
                    "path": skill.path.to_string_lossy(),
                    "enabled": registry.is_enabled(&skill.name),
                })
            })
            .collect(),
    ))
}

async fn skill_detail(
    State(state): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let registry = state.agent.skills();
    let skill = registry
        .get(&name)
        .ok_or((StatusCode::NOT_FOUND, format!("技能不存在：{name}")))?;
    let content = std::fs::read_to_string(&skill.path)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(json!({
        "name": skill.name,
        "description": skill.description,
        "path": skill.path.to_string_lossy(),
        "enabled": registry.is_enabled(&name),
        "content": content,
    })))
}

#[derive(serde::Deserialize)]
struct SkillEditRequest {
    content: String,
}

async fn skill_edit(
    State(state): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
    Json(request): Json<SkillEditRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let skill = state
        .agent
        .skills()
        .get(&name)
        .ok_or((StatusCode::NOT_FOUND, format!("技能不存在：{name}")))?;
    std::fs::write(&skill.path, &request.content)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            "skills",
            "edit",
            Some(name.clone()),
            Some(true),
            "SKILL.md 已更新",
        );
    }
    Ok(Json(json!({
        "ok": true,
        "note": "SKILL.md 已更新（注册表内技能重启核心服务后生效）",
    })))
}

#[derive(serde::Deserialize)]
struct SkillEnabledRequest {
    enabled: bool,
}

async fn skill_enabled(
    State(state): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
    Json(request): Json<SkillEnabledRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let registry = state.agent.skills();
    if registry.get(&name).is_none() {
        return Err((StatusCode::NOT_FOUND, format!("技能不存在：{name}")));
    }
    let disabled = registry.disabled_set();
    {
        let mut set = disabled.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "禁用集合锁中毒".to_string(),
            )
        })?;
        if request.enabled {
            set.remove(&name);
        } else {
            set.insert(name.clone());
        }
    }
    let mut settings = owo_agent_core::Settings::load(&state.workspace);
    let mut list = {
        let set = disabled.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "禁用集合锁中毒".to_string(),
            )
        })?;
        let mut list: Vec<String> = set.iter().cloned().collect();
        list.sort();
        list
    };
    settings.skills.disabled = std::mem::take(&mut list);
    settings
        .save(&state.workspace)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            "skills",
            "enabled",
            Some(name.clone()),
            Some(request.enabled),
            format!(
                "技能{}：{name}",
                if request.enabled { "启用" } else { "禁用" }
            ),
        );
    }
    Ok(Json(json!({
        "ok": true,
        "enabled": request.enabled,
        "note": "已即时生效",
    })))
}

async fn create_session(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateSessionRequest>,
) -> Result<Json<SessionInfo>, (StatusCode, String)> {
    let workspace = std::path::PathBuf::from(&request.workspace);
    if !workspace.is_dir() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("工作区不存在：{}", request.workspace),
        ));
    }
    let model = request.model.unwrap_or_else(|| {
        std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".to_string())
    });
    let session = state
        .store
        .create(&workspace, &model, request.system_prompt.as_deref())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    state
        .sessions
        .lock()
        .map_err(poison)?
        .insert(session.id.clone(), session.clone());
    let title = session.display_title();
    Ok(Json(SessionInfo {
        id: session.id,
        workspace: request.workspace,
        model: session.model,
        created_at: session.created_at,
        updated_at: session.updated_at,
        title: Some(title),
        archived: session.archived,
        pinned: session.pinned,
        parent_id: session.parent_id,
        fork_point: session.fork_point,
    }))
}

async fn get_session(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let session = load_session(&state, &id)?;
    Ok(Json(json!({
        "id": session.id,
        "title": session.display_title(),
        "model": session.model,
        "workspace": session.workspace.to_string_lossy(),
        "created_at": session.created_at,
        "updated_at": session.updated_at,
        "archived": session.archived,
        "pinned": session.pinned,
        "parent_id": session.parent_id,
        "fork_point": session.fork_point,
        "messages": session.messages,
    })))
}

async fn turn(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<TurnRequest>,
) -> Result<Sse<UnboundedReceiverStream<Result<Event, Infallible>>>, (StatusCode, String)> {
    let session = load_session(&state, &id)?;
    let mut effective_prompt = request.prompt.clone();
    if !request.attachments.is_empty() {
        let dir = attachment_dir(&session.workspace, &id);
        let mut lines = Vec::new();
        for attachment in &request.attachments {
            let safe = Path::new(attachment)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(attachment);
            let path = dir.join(safe);
            if !path.is_file() {
                return Err((StatusCode::BAD_REQUEST, format!("附件不存在：{safe}")));
            }
            let size = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
            lines.push(format!(
                "- {}（{} 字节，路径 {}）",
                safe,
                size,
                path.display()
            ));
        }
        effective_prompt.push_str("\n\n附件：\n");
        effective_prompt.push_str(&lines.join("\n"));
    }

    let turn_lock = {
        let mut locks = state.turn_locks.lock().map_err(poison)?;
        locks
            .entry(id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    let turn_guard = turn_lock
        .try_lock_owned()
        .map_err(|_| (StatusCode::CONFLICT, "该会话已有回合正在运行".to_string()))?;
    // R8：全局并发 turn 上限 + 关闭中拒绝新回合。
    let concurrency_permit = state.shutdown_gate.try_acquire_turn().map_err(|busy| {
        // R10：错误码表接入（domain/reason/retryable 统一前缀，见 error_codes.rs）。
        (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("[gateway/unavailable/retryable] {busy}"),
        )
    })?;
    // R8/R9：用量预算硬熔断（Agent 4 交付 usage；超限停轮，错误码贯穿，请求用户加额后恢复）。
    if usage::global().check_budget() {
        let reason = usage::global()
            .hard_stop_reason()
            .unwrap_or_else(|| "用量预算超限".to_string());
        let (status, body) = usage::budget_exceeded_response(&reason);
        let detail = body
            .0
            .get("detail")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&reason)
            .to_string();
        return Err((status, format!("[{}] {detail}", usage::BUDGET_ERROR_CODE)));
    }

    let (tx, rx) = mpsc::unbounded_channel::<Result<Event, Infallible>>();
    let abort_flag = {
        let mut aborts = state.aborts.lock().map_err(poison)?;
        aborts
            .entry(id.clone())
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone()
    };
    abort_flag.store(false, Ordering::Relaxed);
    let approver = ChannelApprover {
        pending: Arc::clone(&state.pending_approvals),
        pending_sessions: Arc::clone(&state.pending_approval_sessions),
        session_id: id.clone(),
        abort: Arc::clone(&abort_flag),
    };

    let agent = Arc::clone(&state.agent);
    let store = Arc::clone(&state.store);
    let sessions = Arc::clone(&state.sessions);
    let traces_dir = state.traces_dir.clone();
    let state_for_audit = Arc::clone(&state);
    tokio::spawn(async move {
        let _turn_guard = turn_guard;
        let _concurrency_permit = concurrency_permit;
        let mut current = session;
        let stream_abort = Arc::clone(&abort_flag);
        let mut on_event = |event: &owo_agent_core::TurnEvent| {
            if let Some(sse) = to_sse(event) {
                if tx.send(to_event(sse)).is_err() {
                    // 客户端断开后尽快停止后续模型/工具调用，避免无主任务继续消耗资源。
                    stream_abort.store(true, Ordering::Relaxed);
                }
            }
        };
        match agent
            .run_turn(
                &mut current,
                &effective_prompt,
                &approver,
                &abort_flag,
                &mut on_event,
            )
            .await
        {
            Ok(outcome) => {
                let trace = owo_agent_core::TraceRecord::from_outcome(&current, &outcome);
                let _ = owo_agent_core::save_trace(&traces_dir, &trace);
                if outcome.usage.total_tokens > 0 {
                    let input_price = std::env::var("OWO_MODEL_INPUT_PRICE_PER_MTOK")
                        .ok()
                        .and_then(|value| value.parse::<f64>().ok())
                        .unwrap_or(0.0);
                    let output_price = std::env::var("OWO_MODEL_OUTPUT_PRICE_PER_MTOK")
                        .ok()
                        .and_then(|value| value.parse::<f64>().ok())
                        .unwrap_or(0.0);
                    let cost = outcome.usage.cost_estimate_usd(input_price, output_price);
                    if let Ok(mut audit) = state_for_audit.agent.audit_log().lock() {
                        audit.record(
                            "model",
                            "usage",
                            Some(current.id.clone()),
                            Some(true),
                            format!(
                                "prompt={} completion={} total={} cost_usd≈{:.6}",
                                outcome.usage.prompt_tokens,
                                outcome.usage.completion_tokens,
                                outcome.usage.total_tokens,
                                cost
                            ),
                        );
                    }
                    // R8：用量与成本归集（Agent 4 交付 usage_router 的会话维度记录）。
                    usage::global().record_tokens(
                        usage::UsageDimension::Session,
                        &current.id,
                        Some(&current.id),
                        outcome.usage.prompt_tokens,
                        outcome.usage.completion_tokens,
                    );
                }
            }
            Err(error) => {
                logging::error(
                    "agent",
                    None,
                    "回合执行失败",
                    &[("session_id", serde_json::json!(current.id))],
                );
                let _ = tx.send(to_event(SseEvent::Progress {
                    message: format!("turn failed: {error}"),
                }));
            }
        }
        if let Ok(mut sessions) = sessions.lock() {
            sessions.insert(current.id.clone(), current.clone());
        }
        if let Err(error) = store.save(&current) {
            let _ = tx.send(to_event(SseEvent::Progress {
                message: format!("session save failed: {error}"),
            }));
        }
        if let Ok(mut aborts) = state_for_audit.aborts.lock() {
            if aborts
                .get(&current.id)
                .is_some_and(|registered| Arc::ptr_eq(registered, &abort_flag))
            {
                aborts.remove(&current.id);
            }
        }
        flush_audit(&state_for_audit);
    });

    Ok(Sse::new(UnboundedReceiverStream::new(rx)))
}

fn attachment_dir(workspace: &Path, session_id: &str) -> std::path::PathBuf {
    workspace.join(".owo-attachments").join(session_id)
}

fn sanitize_attachment_name(name: &str) -> Option<String> {
    let file_name = Path::new(name).file_name()?.to_str()?;
    let cleaned: String = file_name
        .chars()
        .filter(|character| {
            !matches!(
                character,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
            )
        })
        .collect();
    let trimmed = cleaned.trim().to_string();
    if trimmed.is_empty() || trimmed.len() > 200 {
        None
    } else {
        Some(trimmed)
    }
}

#[derive(serde::Deserialize)]
struct AttachmentUploadRequest {
    name: String,
    #[serde(default)]
    mime: Option<String>,
    data_b64: String,
}

async fn attachment_upload(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<AttachmentUploadRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let _session_guard = acquire_session_lock(&state, &id).await?;
    let session = load_session(&state, &id)?;
    let safe_name = sanitize_attachment_name(&request.name)
        .ok_or((StatusCode::BAD_REQUEST, "附件名非法".to_string()))?;
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&request.data_b64)
        .map_err(|error| {
            (
                StatusCode::BAD_REQUEST,
                format!("附件 base64 解码失败：{error}"),
            )
        })?;
    if bytes.len() > 50 * 1024 * 1024 {
        return Err((StatusCode::BAD_REQUEST, "附件超过 50MB 上限".to_string()));
    }
    let dir = attachment_dir(&session.workspace, &id);
    std::fs::create_dir_all(&dir)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let path = dir.join(&safe_name);
    std::fs::write(&path, &bytes)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            &id,
            "attachment",
            Some(safe_name.clone()),
            Some(true),
            format!("上传附件 {}（{} 字节）", safe_name, bytes.len()),
        );
    }
    Ok(Json(json!({
        "id": safe_name,
        "name": request.name,
        "mime": request.mime,
        "size": bytes.len(),
        "path": path.to_string_lossy(),
    })))
}

async fn attachments_list(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Vec<Value>>, (StatusCode, String)> {
    let _session_guard = acquire_session_lock(&state, &id).await?;
    let session = load_session(&state, &id)?;
    let dir = attachment_dir(&session.workspace, &id);
    let mut attachments = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let size = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
            attachments.push(json!({ "id": name, "name": name, "size": size }));
        }
    }
    attachments.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    Ok(Json(attachments))
}

async fn respond_permission(
    State(state): State<Arc<AppState>>,
    AxumPath((session_id, request_id)): AxumPath<(String, String)>,
    Json(response): Json<PermissionResponse>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let belongs_to_session = state
        .pending_approval_sessions
        .lock()
        .map_err(poison)?
        .get(&request_id)
        .map(|pending_session| pending_session == &session_id)
        .unwrap_or(false);
    if !belongs_to_session {
        return Err((
            StatusCode::NOT_FOUND,
            format!("审批请求不存在：{request_id}"),
        ));
    }
    let sender = state
        .pending_approvals
        .lock()
        .map_err(poison)?
        .remove(&request_id)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("审批请求不存在：{request_id}"),
            )
        })?;
    state
        .pending_approval_sessions
        .lock()
        .map_err(poison)?
        .remove(&request_id);
    let decision = if response.allow {
        Decision::Allow
    } else {
        Decision::Deny
    };
    sender
        .send(decision)
        .map_err(|_| (StatusCode::GONE, "审批通道已关闭".to_string()))?;
    Ok(Json(json!({ "ok": true })))
}

async fn abort_turn(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if let Some(flag) = state.aborts.lock().map_err(poison)?.get(&id).cloned() {
        flag.store(true, Ordering::Relaxed);
    }
    Ok(Json(json!({ "ok": true })))
}

async fn diff(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Vec<FileDiff>>, (StatusCode, String)> {
    let session = load_session(&state, &id)?;
    Ok(Json(session.diff()))
}

async fn revert(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let _session_guard = acquire_session_lock(&state, &id).await?;
    let mut session = load_session(&state, &id)?;
    let restored = session
        .revert()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("回滚失败：{e}")))?;
    state
        .store
        .save(&session)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    state
        .sessions
        .lock()
        .map_err(poison)?
        .insert(session.id.clone(), session);
    Ok(Json(json!({ "ok": true, "restored": restored })))
}

async fn fork_session(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<ForkRequest>,
) -> Result<Json<SessionInfo>, (StatusCode, String)> {
    let session = load_session(&state, &id)?;
    let child = session.fork(request.message_index);
    state
        .store
        .save(&child)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    state
        .sessions
        .lock()
        .map_err(poison)?
        .insert(child.id.clone(), child.clone());
    Ok(Json(to_session_info(&child)))
}

async fn rewind_session(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<RewindRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let _session_guard = acquire_session_lock(&state, &id).await?;
    let mut session = load_session(&state, &id)?;
    if request.keep < session.messages.len() {
        session.revert().await.map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("回滚失败：{error}"),
            )
        })?;
    }
    let removed = session.rewind(request.keep);
    state
        .store
        .save(&session)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    state
        .sessions
        .lock()
        .map_err(poison)?
        .insert(session.id.clone(), session);
    Ok(Json(json!({ "ok": true, "removed": removed.len() })))
}

async fn redo_session(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let _session_guard = acquire_session_lock(&state, &id).await?;
    let mut session = load_session(&state, &id)?;
    let restored = session.redo().map(|tail| tail.len()).unwrap_or(0);
    state
        .store
        .save(&session)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    state
        .sessions
        .lock()
        .map_err(poison)?
        .insert(session.id.clone(), session);
    Ok(Json(json!({ "ok": true, "restored": restored })))
}

#[derive(serde::Deserialize)]
struct RenameRequest {
    title: String,
}

#[derive(serde::Deserialize)]
struct ArchiveRequest {
    archived: bool,
}

#[derive(serde::Deserialize)]
struct PinRequest {
    pinned: bool,
}

async fn session_rename(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<RenameRequest>,
) -> Result<Json<SessionInfo>, (StatusCode, String)> {
    let _session_guard = acquire_session_lock(&state, &id).await?;
    let mut session = load_session(&state, &id)?;
    session.rename(request.title);
    state
        .store
        .save(&session)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    state
        .sessions
        .lock()
        .map_err(poison)?
        .insert(session.id.clone(), session.clone());
    Ok(Json(to_session_info(&session)))
}

async fn session_archive(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<ArchiveRequest>,
) -> Result<Json<SessionInfo>, (StatusCode, String)> {
    let _session_guard = acquire_session_lock(&state, &id).await?;
    let mut session = load_session(&state, &id)?;
    session.set_archived(request.archived);
    state
        .store
        .save(&session)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    state
        .sessions
        .lock()
        .map_err(poison)?
        .insert(session.id.clone(), session.clone());
    Ok(Json(to_session_info(&session)))
}

async fn session_pin(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<PinRequest>,
) -> Result<Json<SessionInfo>, (StatusCode, String)> {
    let _session_guard = acquire_session_lock(&state, &id).await?;
    let mut session = load_session(&state, &id)?;
    session.set_pinned(request.pinned);
    state
        .store
        .save(&session)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    state
        .sessions
        .lock()
        .map_err(poison)?
        .insert(session.id.clone(), session.clone());
    Ok(Json(to_session_info(&session)))
}

async fn children(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Vec<SessionInfo>>, (StatusCode, String)> {
    let mut result = Vec::new();
    for session_id in state.store.list() {
        if let Ok(session) = state.store.load(&session_id) {
            if session.parent_id.as_deref() == Some(id.as_str()) {
                result.push(to_session_info(&session));
            }
        }
    }
    Ok(Json(result))
}

async fn export_session(
    State(state): State<Arc<AppState>>,
    AxumPath((id, format)): AxumPath<(String, String)>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    let session = load_session(&state, &id)?;
    let (body, content_type) = match format.as_str() {
        "md" | "markdown" => (
            owo_agent_core::export_markdown(&session),
            "text/markdown; charset=utf-8",
        ),
        "html" => (
            owo_agent_core::export_html(&session),
            "text/html; charset=utf-8",
        ),
        _ => return Err((StatusCode::BAD_REQUEST, "格式仅支持 md / html".to_string())),
    };
    Ok((
        axum::http::StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, content_type)],
        body,
    )
        .into_response())
}

async fn run_eval(
    State(state): State<Arc<AppState>>,
    Json(request): Json<EvalRunRequest>,
) -> Result<Json<owo_agent_core::EvalReport>, (StatusCode, String)> {
    let suite = match request.suite_id.as_str() {
        "builtin" | "builtin-demo" => owo_agent_core::builtin_suite(),
        _ => {
            return Err((
                StatusCode::NOT_FOUND,
                format!("未知评估套件：{}", request.suite_id),
            ))
        }
    };
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".to_string());
    let provider = state.agent.provider();
    let report = owo_agent_core::run_suite(provider, &model, &suite).await;
    Ok(Json(report))
}

// ---------- v0.4 接口 ----------

async fn context_snapshot(
    State(state): State<Arc<AppState>>,
) -> Result<Json<SituationSnapshot>, (StatusCode, String)> {
    let mut perception = state.perception.lock().map_err(poison)?;
    let _ = perception.refresh_from_platform();
    let sequence = owo_agent_core::clipboard_sequence();
    perception.refresh_clipboard(sequence);
    let _ = perception.refresh_from_uia(2, 64);
    Ok(Json(perception.snapshot()))
}

/// perception.subscribe：订阅 L0/L1 事件流（SSE），桌面端感知状态区使用。
async fn perception_events(
    State(state): State<Arc<AppState>>,
) -> Result<Sse<ReceiverStream<Result<Event, Infallible>>>, (StatusCode, String)> {
    let mut perception = state.perception.lock().map_err(poison)?;
    let _ = perception.refresh_from_platform();
    let mut receiver = perception.subscribe();
    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(128);
    tokio::spawn(async move {
        while let Some(event) = receiver.recv().await {
            let data = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
            if tx
                .send(Ok(Event::default().event("perception").data(data)))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    Ok(Sse::new(ReceiverStream::new(rx)))
}

/// L2 按需采集：截图 + 本地 OCR 摘要进内存环形缓冲（不落盘）。
async fn perception_capture(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CaptureRequest>,
) -> Result<Json<owo_agent_core::CaptureMeta>, (StatusCode, String)> {
    let mut perception = state.perception.lock().map_err(poison)?;
    let frame = match (request.width, request.height) {
        (Some(width), Some(height)) => perception
            .begin_capture_region(width, height)
            .map_err(|error| (StatusCode::BAD_REQUEST, error))?,
        _ => perception
            .begin_capture_from_screen()
            .map_err(|error| (StatusCode::BAD_REQUEST, error))?,
    };
    Ok(Json(frame))
}

#[derive(serde::Deserialize)]
struct CaptureRequest {
    #[serde(default)]
    width: Option<i32>,
    #[serde(default)]
    height: Option<i32>,
}

#[derive(serde::Deserialize)]
struct LayersRequest {
    layer: String,
    enabled: bool,
}

/// 感知层级授权开关（L0-L3 逐项授权，可热撤）。
async fn perception_layers(
    State(state): State<Arc<AppState>>,
    Json(request): Json<LayersRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    use owo_agent_core::PerceptionLayer;
    let layer = match request.layer.as_str() {
        "l0_event" => PerceptionLayer::L0Event,
        "l1_ui" => PerceptionLayer::L1Ui,
        "l2_visual" => PerceptionLayer::L2Visual,
        "l3_semantic" => PerceptionLayer::L3Semantic,
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("未知感知层：{other}（l0_event/l1_ui/l2_visual/l3_semantic）"),
            ))
        }
    };
    let mut perception = state.perception.lock().map_err(poison)?;
    perception.set_layer_enabled(layer, request.enabled);
    Ok(Json(
        json!({ "layer": request.layer, "enabled": request.enabled }),
    ))
}

#[derive(serde::Deserialize)]
struct TreeDumpRequest {
    #[serde(default = "default_tree_depth")]
    max_depth: u32,
    #[serde(default = "default_tree_nodes")]
    max_nodes: usize,
    /// 可选：按窗口句柄抓树（不要求前台），用于窗口模板/后台情景理解。
    #[serde(default)]
    hwnd: Option<i64>,
}

fn default_tree_depth() -> u32 {
    12
}

fn default_tree_nodes() -> usize {
    1000
}

/// 深度 UI 树转储（computer-use 调试：找深层语义锚点，如 QQ 工具栏按钮）。
async fn perception_tree(
    State(state): State<Arc<AppState>>,
    Json(request): Json<TreeDumpRequest>,
) -> Result<Json<Vec<owo_agent_core::UiNode>>, (StatusCode, String)> {
    require_perception_layer(&state, owo_agent_core::PerceptionLayer::L1Ui)?;
    let tree = match request.hwnd {
        Some(hwnd) => {
            owo_agent_core::ui_tree_for_hwnd(hwnd as isize, request.max_depth, request.max_nodes)
        }
        None => owo_agent_core::foreground_ui_tree(request.max_depth, request.max_nodes),
    };
    tree.map(Json)
        .ok_or((StatusCode::BAD_REQUEST, "无法获取 UI 树".to_string()))
}

#[derive(serde::Deserialize)]
struct TemplateBuildRequest {
    hwnd: i64,
    app_id: String,
}

async fn perception_template_build(
    State(state): State<Arc<AppState>>,
    Json(request): Json<TemplateBuildRequest>,
) -> Result<Json<owo_agent_core::WindowTemplate>, (StatusCode, String)> {
    require_perception_layer(&state, owo_agent_core::PerceptionLayer::L1Ui)?;
    let tree = owo_agent_core::ui_tree_for_hwnd(request.hwnd as isize, 14, 10000)
        .ok_or((StatusCode::BAD_REQUEST, "无法获取窗口 UI 树".to_string()))?;
    let template = owo_agent_core::build_template(&request.app_id, &tree);
    owo_agent_core::save_template(&state.data_root, &template)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            "template",
            "build",
            Some(request.app_id.clone()),
            Some(true),
            format!(
                "构建窗口模板：{}（{} 个 ROI）",
                request.app_id,
                template.rois.len()
            ),
        );
    }
    Ok(Json(template))
}

async fn perception_template_get(
    State(state): State<Arc<AppState>>,
    AxumPath(app_id): AxumPath<String>,
) -> Result<Json<owo_agent_core::WindowTemplate>, (StatusCode, String)> {
    owo_agent_core::load_template(&state.data_root, &app_id)
        .map(Json)
        .ok_or((StatusCode::NOT_FOUND, format!("窗口模板不存在：{app_id}")))
}

#[derive(serde::Deserialize)]
struct TemplateDetectRequest {
    hwnd: i64,
    app_id: String,
}

async fn perception_template_detect(
    State(state): State<Arc<AppState>>,
    Json(request): Json<TemplateDetectRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    require_perception_layer(&state, owo_agent_core::PerceptionLayer::L1Ui)?;
    let template = owo_agent_core::load_template(&state.data_root, &request.app_id).ok_or((
        StatusCode::NOT_FOUND,
        format!("窗口模板不存在：{}", request.app_id),
    ))?;
    let tree = owo_agent_core::ui_tree_for_hwnd(request.hwnd as isize, 14, 10000)
        .ok_or((StatusCode::BAD_REQUEST, "无法获取窗口 UI 树".to_string()))?;
    Ok(Json(owo_agent_core::detect_template(&template, &tree)))
}

/// OCR 版模板构建：PrintWindow 抓窗口 → PP-OCRv6 → 按语义文本提取 ROI（后台可用）。
async fn perception_template_build_ocr(
    State(state): State<Arc<AppState>>,
    Json(request): Json<TemplateBuildRequest>,
) -> Result<Json<owo_agent_core::WindowTemplate>, (StatusCode, String)> {
    require_perception_layer(&state, owo_agent_core::PerceptionLayer::L2Visual)?;
    let (bmp, _rect) = owo_agent_core::platform::capture_window_bmp_deep(request.hwnd as isize)
        .ok_or((StatusCode::BAD_REQUEST, "窗口截图失败".to_string()))?;
    let summary = owo_agent_core::ocr_preferred(&bmp)
        .await
        .map_err(|error| (StatusCode::BAD_GATEWAY, error))?;
    let template = owo_agent_core::build_template_from_ocr(&request.app_id, &summary);
    owo_agent_core::save_template(&state.data_root, &template)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    Ok(Json(template))
}

/// OCR 版模板检测：当前窗口 OCR 行中心 vs 模板 ROI 命中率。
async fn perception_template_detect_ocr(
    State(state): State<Arc<AppState>>,
    Json(request): Json<TemplateDetectRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    require_perception_layer(&state, owo_agent_core::PerceptionLayer::L2Visual)?;
    let template = owo_agent_core::load_template(&state.data_root, &request.app_id).ok_or((
        StatusCode::NOT_FOUND,
        format!("窗口模板不存在：{}", request.app_id),
    ))?;
    let (bmp, _rect) = owo_agent_core::platform::capture_window_bmp_deep(request.hwnd as isize)
        .ok_or((StatusCode::BAD_REQUEST, "窗口截图失败".to_string()))?;
    let summary = owo_agent_core::ocr_preferred(&bmp)
        .await
        .map_err(|error| (StatusCode::BAD_GATEWAY, error))?;
    Ok(Json(owo_agent_core::detect_template_ocr(
        &template, &summary,
    )))
}

#[derive(serde::Deserialize)]
struct ElementsRequest {
    hwnd: i64,
    app_id: String,
    /// 可选视觉 grounding 结果（vision_ground 的 box + 描述），并入同一注册表。
    #[serde(default)]
    vision: Vec<owo_agent_core::VisionGrounding>,
}

/// 窗口元素注册表：UIA 树 + 窗口 OCR（转屏幕坐标）融合 → 注册表更新 → 返回稳定元素列表。
async fn perception_elements(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ElementsRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    require_perception_layer(&state, owo_agent_core::PerceptionLayer::L1Ui)?;
    require_perception_layer(&state, owo_agent_core::PerceptionLayer::L2Visual)?;
    let tree =
        owo_agent_core::ui_tree_for_hwnd(request.hwnd as isize, 14, 10000).unwrap_or_default();
    let (bmp, rect) = owo_agent_core::platform::capture_window_bmp_deep(request.hwnd as isize)
        .ok_or((StatusCode::BAD_REQUEST, "窗口截图失败".to_string()))?;
    let summary = owo_agent_core::ocr_preferred(&bmp)
        .await
        .map_err(|error| (StatusCode::BAD_GATEWAY, error))?;
    let mut lines = owo_agent_core::group_ocr_lines(&summary.boxes);
    for line in &mut lines {
        line.x += rect.0;
        line.y += rect.1;
    }
    let fused = owo_agent_core::fuse_sources_with_vision(&tree, &lines, &request.vision);
    let mut registry = state.elements.lock().map_err(poison)?;
    let elements = registry.update(&request.app_id, fused);
    Ok(Json(json!({
        "app_id": request.app_id,
        "provider": summary.provider,
        "count": elements.len(),
        "elements": elements,
    })))
}

/// 全屏 OCR（含文本框坐标），供 OCR+坐标点击（自绘面板，如 QQ 红包/表情）。
async fn perception_ocr(
    State(state): State<Arc<AppState>>,
) -> Result<Json<owo_agent_core::OcrSummary>, (StatusCode, String)> {
    if !state
        .perception
        .lock()
        .map_err(poison)?
        .is_enabled(owo_agent_core::PerceptionLayer::L2Visual)
    {
        return Err((StatusCode::BAD_REQUEST, "L2 视觉层未授权".to_string()));
    }
    let bytes = owo_agent_core::capture_screen()
        .ok_or((StatusCode::BAD_REQUEST, "屏幕截图失败".to_string()))?;
    owo_agent_core::ocr_preferred(&bytes)
        .await
        .map(Json)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))
}

async fn ocr_status() -> Json<owo_agent_core::OcrEngineStatus> {
    Json(owo_agent_core::ocr_engine_status())
}

#[derive(serde::Deserialize)]
struct OcrBytesRequest {
    bmp_b64: String,
}

/// 对 base64 编码的 BMP 做 OCR（模拟窗口帧/附件截图调试用，不依赖屏幕）。
async fn perception_ocr_bytes(
    Json(request): Json<OcrBytesRequest>,
) -> Result<Json<owo_agent_core::OcrSummary>, (StatusCode, String)> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&request.bmp_b64)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("base64 解码失败：{e}")))?;
    owo_agent_core::ocr_preferred(&bytes)
        .await
        .map(Json)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))
}

#[derive(serde::Deserialize)]
struct OcrRegionRequest {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    #[serde(default = "default_ocr_scale")]
    scale: u32,
}

fn default_ocr_scale() -> u32 {
    2
}

fn default_true() -> bool {
    true
}

/// 区域 OCR：裁剪 + 放大后识别（小字验证窗口/自绘面板）。
async fn perception_ocr_region(
    State(state): State<Arc<AppState>>,
    Json(request): Json<OcrRegionRequest>,
) -> Result<Json<owo_agent_core::OcrSummary>, (StatusCode, String)> {
    if !state
        .perception
        .lock()
        .map_err(poison)?
        .is_enabled(owo_agent_core::PerceptionLayer::L2Visual)
    {
        return Err((StatusCode::BAD_REQUEST, "L2 视觉层未授权".to_string()));
    }
    let bytes = owo_agent_core::capture_screen()
        .ok_or((StatusCode::BAD_REQUEST, "屏幕截图失败".to_string()))?;
    let cropped = owo_agent_core::crop_scale_bmp(
        &bytes,
        request.x,
        request.y,
        request.width,
        request.height,
        request.scale,
    )
    .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    owo_agent_core::ocr_preferred(&cropped)
        .await
        .map(Json)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))
}

#[derive(serde::Deserialize)]
struct WindowOcrRequest {
    hwnd: i64,
}

/// 窗口级 OCR：PrintWindow 后台只读抓取指定窗口 → PP-OCRv6/Media 识别，返回窗口矩形与文本行。
async fn perception_window(
    State(state): State<Arc<AppState>>,
    Json(request): Json<WindowOcrRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    require_perception_layer(&state, owo_agent_core::PerceptionLayer::L2Visual)?;
    let (bmp, rect) = owo_agent_core::platform::capture_window_bmp_deep(request.hwnd as isize)
        .ok_or((StatusCode::BAD_REQUEST, "窗口截图失败".to_string()))?;
    let summary = owo_agent_core::ocr_preferred(&bmp)
        .await
        .map_err(|error| (StatusCode::BAD_GATEWAY, error))?;
    let lines: Vec<Value> = owo_agent_core::group_ocr_lines(&summary.boxes)
        .into_iter()
        .map(|line| {
            json!({
                "text": line.text,
                "x": line.x,
                "y": line.y,
                "width": line.width,
                "height": line.height,
            })
        })
        .collect();
    Ok(Json(json!({
        "window_rect": [rect.0, rect.1, rect.2, rect.3],
        "provider": summary.provider,
        "chars": summary.chars,
        "text": summary.text,
        "lines": lines,
        "boxes": summary.boxes,
    })))
}

#[derive(serde::Deserialize)]
struct SensitiveProbe {
    #[serde(default)]
    name: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    ocr_text: String,
}

#[derive(serde::Deserialize)]
struct DesktopClickRequest {
    x: i32,
    y: i32,
    /// 可选：computer-use 任务 id，提供时动作先过门禁（未批准/越界应用/敏感熔断拒绝）。
    #[serde(default)]
    task_id: Option<String>,
    /// 可选：敏感 UI 探针（UI 属性/名称/OCR 关键词），门禁内熔断判定。
    #[serde(default)]
    sensitive: Option<SensitiveProbe>,
}

#[derive(serde::Deserialize)]
struct DesktopTextRequest {
    text: String,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    sensitive: Option<SensitiveProbe>,
}

#[derive(serde::Deserialize)]
struct DesktopKeyRequest {
    key: String,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    sensitive: Option<SensitiveProbe>,
}

#[derive(serde::Deserialize)]
struct DesktopComboRequest {
    combo: String,
}

#[derive(serde::Deserialize)]
struct DesktopTargetRequest {
    target: String,
    #[serde(default)]
    task_id: Option<String>,
}

#[derive(serde::Deserialize)]
struct DesktopScrollRequest {
    x: i32,
    y: i32,
    delta: i32,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    sensitive: Option<SensitiveProbe>,
}

#[derive(serde::Deserialize)]
struct DesktopActivateRequest {
    #[serde(default)]
    process: String,
    #[serde(default)]
    title: String,
}

#[derive(serde::Deserialize)]
struct DesktopWaitRequest {
    ms: u64,
}

async fn desktop_foreground() -> Json<Value> {
    if std::env::var("OWO_SIM_QQ_URL")
        .map(|value| !value.is_empty())
        .unwrap_or(false)
    {
        return Json(json!({
            "process": "owo-sim-qq",
            "title": "OwO 模拟QQ - 张子豪",
            "rect": [0, 0, 1020, 700],
            "surface": "sim",
        }));
    }
    let (process, title) = owo_agent_core::poll_foreground_app().unwrap_or_default();
    let rect = owo_agent_core::platform::foreground_window_rect();
    Json(json!({ "process": process, "title": title, "rect": rect }))
}

async fn desktop_windows() -> Json<Value> {
    if std::env::var("OWO_SIM_QQ_URL")
        .map(|value| !value.is_empty())
        .unwrap_or(false)
    {
        return Json(json!({
            "windows": [{
                "hwnd": 1,
                "pid": 1,
                "process": "owo-sim-qq",
                "title": "OwO 模拟QQ - 张子豪",
                "rect": [0, 0, 1020, 700],
                "visible": true,
            }],
            "surface": "sim",
        }));
    }
    Json(json!({ "windows": owo_agent_core::platform::window_list() }))
}

/// 可选 task_id 门禁：请求携带 task_id 时，动作执行前先过 computer-use 门禁
/// （状态/超时/允许集/目标应用/敏感熔断/预算），拒绝返回 403 并写审计。
fn gate_desktop_action(
    state: &AppState,
    task_id: &Option<String>,
    action: &str,
    probe: &Option<SensitiveProbe>,
) -> Result<(), (StatusCode, String)> {
    let Some(task_id) = task_id else {
        return Ok(());
    };
    let app = owo_agent_core::platform::poll_foreground_app()
        .map(|(app_id, _)| app_id)
        .unwrap_or_default();
    let sensitive = probe
        .as_ref()
        .map(|p| (p.name.as_str(), p.role.as_str(), p.ocr_text.as_str()));
    let audit = state.agent.audit_log();
    let mut log = audit
        .lock()
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "审计锁中毒".to_string()))?;
    owo_agent_core::computer_use::task_gate_check(
        &state.computer_tasks,
        Some(&mut log),
        "computer-use",
        task_id,
        action,
        &app,
        sensitive,
    )
    .map_err(|error| (StatusCode::FORBIDDEN, error))
}

async fn desktop_activate(
    Json(request): Json<DesktopActivateRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    ensure_real_desktop("desktop_activate")?;
    owo_agent_core::platform::activate_window(&request.process, &request.title)
        .map(|_| Json(json!({ "ok": true })))
        .map_err(|error| (StatusCode::BAD_REQUEST, error))
}

async fn desktop_click(
    State(state): State<Arc<AppState>>,
    Json(request): Json<DesktopClickRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    ensure_real_desktop("desktop_click")?;
    gate_desktop_action(
        &state,
        &request.task_id,
        "desktop_click",
        &request.sensitive,
    )?;
    owo_agent_core::computer_use::desktop_click(request.x, request.y)
        .map(|_| Json(json!({ "ok": true, "x": request.x, "y": request.y })))
        .map_err(|error| (StatusCode::BAD_REQUEST, error))
}

async fn desktop_type(
    State(state): State<Arc<AppState>>,
    Json(request): Json<DesktopTextRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    ensure_real_desktop("desktop_type")?;
    gate_desktop_action(&state, &request.task_id, "desktop_type", &request.sensitive)?;
    owo_agent_core::computer_use::desktop_type(&request.text)
        .map(|_| Json(json!({ "ok": true, "typed_chars": request.text.chars().count() })))
        .map_err(|error| (StatusCode::BAD_REQUEST, error))
}

async fn desktop_key(
    State(state): State<Arc<AppState>>,
    Json(request): Json<DesktopKeyRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    ensure_real_desktop("desktop_key")?;
    gate_desktop_action(&state, &request.task_id, "desktop_key", &request.sensitive)?;
    owo_agent_core::computer_use::desktop_key(&request.key)
        .map(|_| Json(json!({ "ok": true, "key": request.key })))
        .map_err(|error| (StatusCode::BAD_REQUEST, error))
}

async fn desktop_shortcut(
    Json(request): Json<DesktopComboRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    ensure_real_desktop("desktop_shortcut")?;
    owo_agent_core::computer_use::desktop_shortcut(&request.combo)
        .map(|_| Json(json!({ "ok": true, "combo": request.combo })))
        .map_err(|error| (StatusCode::BAD_REQUEST, error))
}

async fn desktop_launch(
    State(state): State<Arc<AppState>>,
    Json(request): Json<DesktopTargetRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    ensure_real_desktop("desktop_launch")?;
    gate_desktop_action(&state, &request.task_id, "desktop_launch", &None)?;
    owo_agent_core::computer_use::desktop_launch(&request.target)
        .map(|_| Json(json!({ "ok": true, "target": request.target })))
        .map_err(|error| (StatusCode::BAD_REQUEST, error))
}

async fn desktop_scroll(
    State(state): State<Arc<AppState>>,
    Json(request): Json<DesktopScrollRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    ensure_real_desktop("desktop_scroll")?;
    gate_desktop_action(
        &state,
        &request.task_id,
        "desktop_scroll",
        &request.sensitive,
    )?;
    owo_agent_core::computer_use::desktop_scroll(request.x, request.y, request.delta)
        .map(|_| {
            Json(json!({ "ok": true, "x": request.x, "y": request.y, "delta": request.delta }))
        })
        .map_err(|error| (StatusCode::BAD_REQUEST, error))
}

async fn desktop_wait(Json(request): Json<DesktopWaitRequest>) -> Json<Value> {
    let ms = request.ms.min(120_000);
    tokio::time::sleep(Duration::from_millis(ms)).await;
    Json(json!({ "waited_ms": ms }))
}

async fn vision_status() -> Json<Value> {
    let config = owo_agent_core::VisionConfig::from_env();
    let models = if config.provider == "ollama" {
        owo_agent_core::ollama_models(&config).await
    } else {
        Vec::new()
    };
    Json(json!({
        "provider": config.provider,
        "model": config.model,
        "ollama_host": config.ollama_host,
        "ollama_models": models,
    }))
}

#[derive(serde::Deserialize)]
struct VisionDescribeRequest {
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    x: Option<i32>,
    #[serde(default)]
    y: Option<i32>,
    #[serde(default)]
    width: Option<i32>,
    #[serde(default)]
    height: Option<i32>,
    #[serde(default)]
    scale: Option<u32>,
}

async fn vision_describe(
    State(state): State<Arc<AppState>>,
    Json(request): Json<VisionDescribeRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    require_perception_layer(&state, owo_agent_core::PerceptionLayer::L2Visual)?;
    let (png, surface) = match (request.x, request.y, request.width, request.height) {
        (Some(x), Some(y), Some(width), Some(height)) => owo_agent_core::capture_vision_png_region(
            x,
            y,
            width,
            height,
            request.scale.unwrap_or(3),
        )
        .await
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?,
        _ => owo_agent_core::capture_vision_png()
            .await
            .map_err(|error| (StatusCode::BAD_REQUEST, error))?,
    };
    let prompt = request.prompt.unwrap_or_else(|| {
        "请用中文描述这个界面的当前状态：这是什么应用？有哪些关键控件（按钮/输入框/消息）？\
         它们大致在什么位置？最新消息内容是什么？"
            .to_string()
    });
    let description = owo_agent_core::describe_image(&png, &prompt)
        .await
        .map_err(|error| (StatusCode::BAD_GATEWAY, error))?;
    let config = owo_agent_core::VisionConfig::from_env();
    Ok(Json(json!({
        "surface": surface,
        "provider": config.provider,
        "model": config.model,
        "description": description,
    })))
}

#[derive(serde::Deserialize)]
struct VisionVerifyRequest {
    question: String,
    /// 是否忽略输入框占位文字（默认 true）。
    #[serde(default = "default_true")]
    ignore_placeholder: bool,
    #[serde(default)]
    x: Option<i32>,
    #[serde(default)]
    y: Option<i32>,
    #[serde(default)]
    width: Option<i32>,
    #[serde(default)]
    height: Option<i32>,
    #[serde(default)]
    scale: Option<u32>,
}

/// 视觉完成验证：对当前截图回答 yes/no 问题，返回 answer + confidence。
async fn vision_verify(
    State(state): State<Arc<AppState>>,
    Json(request): Json<VisionVerifyRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    require_perception_layer(&state, owo_agent_core::PerceptionLayer::L2Visual)?;
    let (png, surface) = match (request.x, request.y, request.width, request.height) {
        (Some(x), Some(y), Some(width), Some(height)) => owo_agent_core::capture_vision_png_region(
            x,
            y,
            width,
            height,
            request.scale.unwrap_or(3),
        )
        .await
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?,
        _ => owo_agent_core::capture_vision_png()
            .await
            .map_err(|error| (StatusCode::BAD_REQUEST, error))?,
    };
    let prompt = owo_agent_core::verification_prompt(&request.question, request.ignore_placeholder);
    let raw = owo_agent_core::describe_image(&png, &prompt)
        .await
        .map_err(|error| (StatusCode::BAD_GATEWAY, error))?;
    let (answer, confidence) = owo_agent_core::parse_verification(&raw);
    let config = owo_agent_core::VisionConfig::from_env();
    Ok(Json(json!({
        "surface": surface,
        "provider": config.provider,
        "model": config.model,
        "question": request.question,
        "answer": answer,
        "confidence": confidence,
        "raw": raw,
    })))
}

#[derive(serde::Deserialize)]
struct VisionGroundRequest {
    description: String,
    /// 可选：应用标识，提供时 grounding 结果写入窗口元素注册表并返回 element_id。
    #[serde(default)]
    app_id: Option<String>,
}

/// 视觉 grounding：视觉模型给框 → 与 OCR 文本交叉验证；
/// 无 OCR 文本时仅高置信度（≥0.9）标记 vision_only 允许纯视觉定位。
async fn vision_ground(
    State(state): State<Arc<AppState>>,
    Json(request): Json<VisionGroundRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    require_perception_layer(&state, owo_agent_core::PerceptionLayer::L2Visual)?;
    let mut result = owo_agent_core::ground_element(&request.description)
        .await
        .map_err(|error| (StatusCode::BAD_GATEWAY, error))?;
    if let Some(app_id) = request.app_id {
        if result
            .get("matched")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            let grounding = owo_agent_core::computer_use::vision_grounding_from_value(
                &result,
                &request.description,
            )
            .map_err(|error| (StatusCode::BAD_GATEWAY, error))?;
            let mut registry = state.elements.lock().map_err(poison)?;
            if let Some(element_id) =
                owo_agent_core::register_vision_grounding(&mut registry, &app_id, grounding)
            {
                result["element_id"] = serde_json::json!(element_id);
                result["app_id"] = serde_json::json!(app_id);
            }
        }
    }
    Ok(Json(result))
}

async fn memory_observations(
    State(state): State<Arc<AppState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let limit = params
        .get("limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100);
    let memory = state.memory.lock().map_err(poison)?;
    let observations = memory.list(limit);
    Ok(Json(json!({
        "count": observations.len(),
        "total": memory.count(),
        "observations": observations,
    })))
}

async fn memory_clear(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut memory = state.memory.lock().map_err(poison)?;
    memory
        .clear()
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(serde::Deserialize)]
struct MineSkillRequest {
    name: String,
    target_apps: Vec<String>,
    sensitivity: String,
    description: String,
}

/// 从情景记忆自动挖掘流程技能：观察到的动作序列 → 泛化 → 沉淀技能包。
async fn memory_mine_skill(
    State(state): State<Arc<AppState>>,
    Json(request): Json<MineSkillRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let sensitivity = parse_sensitivity(&request.sensitivity)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    let actions = {
        let memory = state.memory.lock().map_err(poison)?;
        let observations = memory.list(0);
        owo_agent_core::map_sim_events_to_actions(&observations)
    };
    if actions.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "情景记忆中没有可挖掘的动作（请先运行模拟/真实操作并等待观察器入库）".to_string(),
        ));
    }
    let mut pipeline = state.pipeline.lock().map_err(poison)?;
    pipeline.recorder.start();
    for action in actions {
        pipeline
            .recorder
            .record(action)
            .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    }
    let package = pipeline
        .sink_skill(
            &request.name,
            request.target_apps,
            sensitivity,
            &request.description,
        )
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            "memory",
            "mine-skill",
            Some(package.manifest.name.clone()),
            Some(true),
            format!("从情景记忆挖掘技能包：{}", package.manifest.name),
        );
    }
    Ok(Json(json!({
        "ok": true,
        "name": package.manifest.name,
        "variables": package.manifest.variables,
    })))
}

fn ensure_real_desktop(tool: &str) -> Result<(), (StatusCode, String)> {
    if std::env::var("OWO_SIM_QQ_URL")
        .map(|value| !value.is_empty())
        .unwrap_or(false)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("{tool} 在模拟环境下被禁用：请直连模拟服务或通过 Agent 工具执行",),
        ));
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct LearnRecordRequest {
    action: RecordedAction,
}

async fn learn_start(
    State(state): State<Arc<AppState>>,
) -> Result<Json<LearnState>, (StatusCode, String)> {
    let mut pipeline = state.pipeline.lock().map_err(poison)?;
    pipeline.recorder.start();
    Ok(Json(pipeline.recorder.state()))
}

async fn learn_record(
    State(state): State<Arc<AppState>>,
    Json(request): Json<LearnRecordRequest>,
) -> Result<Json<LearnState>, (StatusCode, String)> {
    let mut pipeline = state.pipeline.lock().map_err(poison)?;
    pipeline
        .recorder
        .record(request.action)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    Ok(Json(pipeline.recorder.state()))
}

async fn learn_pause(
    State(state): State<Arc<AppState>>,
) -> Result<Json<LearnState>, (StatusCode, String)> {
    let mut pipeline = state.pipeline.lock().map_err(poison)?;
    pipeline.recorder.pause();
    Ok(Json(pipeline.recorder.state()))
}

async fn learn_resume(
    State(state): State<Arc<AppState>>,
) -> Result<Json<LearnState>, (StatusCode, String)> {
    let mut pipeline = state.pipeline.lock().map_err(poison)?;
    pipeline.recorder.resume();
    Ok(Json(pipeline.recorder.state()))
}

async fn learn_stop(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut pipeline = state.pipeline.lock().map_err(poison)?;
    let samples = pipeline.stop_recording().len();
    Ok(Json(json!({
        "state": pipeline.recorder.state(),
        "samples": samples,
    })))
}

async fn learn_clear(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut pipeline = state.pipeline.lock().map_err(poison)?;
    pipeline.recorder.clear();
    Ok(Json(json!({ "ok": true })))
}

async fn learn_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pipeline = state.pipeline.lock().map_err(poison)?;
    Ok(Json(json!({
        "state": pipeline.recorder.state(),
        "samples": pipeline.recorder.samples(),
        "sensitive_break": pipeline.recorder.sensitive_break(),
    })))
}

#[derive(serde::Deserialize)]
struct ExecuteRequest {
    graph: owo_agent_core::ActionGraph,
    #[serde(default)]
    variables: std::collections::HashMap<String, String>,
    #[serde(default)]
    max_steps: Option<usize>,
    /// 首次执行必须显式确认（服务端强制审批）。
    #[serde(default)]
    confirm: bool,
}

/// 执行流程技能包动作图（Windows：UI Automation + SendInput，敏感面熔断）。
async fn learn_execute(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ExecuteRequest>,
) -> Result<Json<owo_agent_core::ExecReport>, (StatusCode, String)> {
    if !request.confirm {
        return Err((
            StatusCode::BAD_REQUEST,
            "首次执行必须确认（confirm: true）".to_string(),
        ));
    }
    let source = ui_action_source(state.elements.clone())?;
    let report = owo_agent_core::execute_graph(
        source.as_ref(),
        &request.graph,
        &request.variables,
        request.max_steps.unwrap_or(20),
    );
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        for step in &report.steps {
            audit.record(
                "learn-execute",
                "exec",
                Some(step.node_id.clone()),
                Some(step.status == "ok"),
                step.detail.clone(),
            );
        }
    }
    Ok(Json(report))
}

fn parse_sensitivity(value: &str) -> Result<Sensitivity, String> {
    match value {
        "low" => Ok(Sensitivity::Low),
        "medium" => Ok(Sensitivity::Medium),
        "high" => Ok(Sensitivity::High),
        "none" => Ok(Sensitivity::None),
        other => Err(format!("未知敏感度：{other}（low/medium/high/none）")),
    }
}

/// 流程技能包列表（用户学习产物）。
async fn learn_packages(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<Value>>, (StatusCode, String)> {
    let pipeline = state.pipeline.lock().map_err(poison)?;
    let mut packages = Vec::new();
    for name in pipeline
        .store
        .list()
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?
    {
        if let Ok(package) = pipeline.store.load(&name) {
            packages.push(json!({
                "name": package.manifest.name,
                "target_apps": package.manifest.target_apps,
                "variables": package.manifest.variables,
                "sensitivity": package.manifest.sensitivity,
                "version": package.manifest.version,
                "health": pipeline.store.health_state(&name),
            }));
        }
    }
    Ok(Json(packages))
}

async fn learn_package_detail(
    State(state): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pipeline = state.pipeline.lock().map_err(poison)?;
    let package = pipeline
        .store
        .load(&name)
        .map_err(|error| (StatusCode::NOT_FOUND, error))?;
    Ok(Json(json!({
        "name": package.manifest.name,
        "target_apps": package.manifest.target_apps,
        "variables": package.manifest.variables,
        "sensitivity": package.manifest.sensitivity,
        "version": package.manifest.version,
        "skill_md": package.skill_md,
        "graph": package.graph,
    })))
}

async fn learn_package_delete(
    State(state): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pipeline = state.pipeline.lock().map_err(poison)?;
    pipeline
        .store
        .delete(&name)
        .map_err(|error| (StatusCode::NOT_FOUND, error))?;
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            "learn",
            "delete-package",
            Some(name.clone()),
            Some(true),
            format!("删除流程技能包：{name}"),
        );
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(serde::Deserialize)]
struct SinkRequest {
    name: String,
    target_apps: Vec<String>,
    sensitivity: String,
    description: String,
}

/// 结束录制并沉淀为流程技能包。
async fn learn_sink(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SinkRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let sensitivity = parse_sensitivity(&request.sensitivity)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    let mut pipeline = state.pipeline.lock().map_err(poison)?;
    let package = pipeline
        .sink_skill(
            &request.name,
            request.target_apps,
            sensitivity,
            &request.description,
        )
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    Ok(Json(json!({
        "ok": true,
        "name": package.manifest.name,
        "variables": package.manifest.variables,
    })))
}

#[derive(serde::Deserialize)]
struct ExecutePackageRequest {
    name: String,
    #[serde(default)]
    variables: HashMap<String, String>,
    #[serde(default)]
    max_steps: Option<usize>,
    /// 首次执行必须显式确认（服务端强制审批）。
    #[serde(default)]
    confirm: bool,
    /// 高敏感（High）技能包需二次确认。
    #[serde(default)]
    high_risk_ack: bool,
}

/// 从流程技能包加载动作图并执行（首次执行需在 UI 确认，步审计入库）。
async fn learn_execute_package(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ExecutePackageRequest>,
) -> Result<Json<owo_agent_core::ExecReport>, (StatusCode, String)> {
    if !request.confirm {
        return Err((
            StatusCode::BAD_REQUEST,
            "首次执行必须确认（confirm: true）".to_string(),
        ));
    }
    let package = {
        let pipeline = state.pipeline.lock().map_err(poison)?;
        pipeline
            .store
            .execution_gate(&request.name, false)
            .map_err(|error| (StatusCode::CONFLICT, error))?;
        pipeline
            .store
            .load(&request.name)
            .map_err(|error| (StatusCode::NOT_FOUND, error))?
    };
    if package.manifest.sensitivity == Sensitivity::High && !request.high_risk_ack {
        return Err((
            StatusCode::BAD_REQUEST,
            "高敏感技能包需二次确认（high_risk_ack: true）".to_string(),
        ));
    }
    let source = ui_action_source(state.elements.clone())?;
    let report = owo_agent_core::execute_graph(
        source.as_ref(),
        &package.graph,
        &request.variables,
        request.max_steps.unwrap_or(20),
    );
    {
        let pipeline = state.pipeline.lock().map_err(poison)?;
        let failed = report.steps.iter().find(|step| step.status != "ok");
        let _ = pipeline.store.record_execution(
            &request.name,
            report.ok,
            failed
                .map(|step| step.node_id.as_str())
                .unwrap_or("completed"),
            failed.map(|step| step.detail.as_str()).unwrap_or(""),
        );
    }
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        if package.manifest.sensitivity == Sensitivity::High {
            audit.record(
                "learn-execute-package",
                "high_risk_ack",
                Some(request.name.clone()),
                Some(true),
                "高敏感技能包二次确认",
            );
        }
        audit.record(
            "learn-execute-package",
            "approval",
            Some(request.name.clone()),
            Some(true),
            "首次执行已确认",
        );
        for step in &report.steps {
            audit.record(
                "learn-execute-package",
                "exec",
                Some(step.node_id.clone()),
                Some(step.status == "ok"),
                step.detail.clone(),
            );
        }
    }
    Ok(Json(report))
}

/// 根据运行环境选择执行器源：模拟面用 SimUiActionSource（虚拟窗口），
/// 真实桌面用 WindowsUiaSource。
fn ui_action_source(
    elements: std::sync::Arc<std::sync::Mutex<owo_agent_core::ElementRegistry>>,
) -> Result<Box<dyn owo_agent_core::UiActionSource>, (StatusCode, String)> {
    if std::env::var("OWO_SIM_QQ_URL")
        .map(|value| !value.is_empty())
        .unwrap_or(false)
    {
        owo_agent_core::computer_use::SimUiActionSource::new()
            .map(|source| Box::new(source) as Box<dyn owo_agent_core::UiActionSource>)
            .map_err(|error| (StatusCode::BAD_REQUEST, error))
    } else {
        owo_agent_core::WindowsUiaSource::new_with_registry(Some(elements))
            .map(|source| Box::new(source) as Box<dyn owo_agent_core::UiActionSource>)
            .map_err(|error| (StatusCode::BAD_REQUEST, error))
    }
}

/// 导出流程技能包为 `.owskill`（ZIP）。
async fn learn_export(
    State(state): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    let package = {
        let pipeline = state.pipeline.lock().map_err(poison)?;
        pipeline
            .store
            .load(&name)
            .map_err(|error| (StatusCode::NOT_FOUND, error))?
    };
    let bytes = owo_agent_core::export_flow_skill_package(&package)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    let disposition = format!("attachment; filename=\"{name}.owskill\"");
    Ok((
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/zip".to_string(),
            ),
            (axum::http::header::CONTENT_DISPOSITION, disposition),
        ],
        bytes,
    )
        .into_response())
}

/// 导入 `.owskill`（ZIP）并保存到用户技能包目录。
async fn learn_import(
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, String)> {
    let package = owo_agent_core::import_flow_skill_package(&body)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    let pipeline = state.pipeline.lock().map_err(poison)?;
    pipeline
        .store
        .save(&package)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    Ok(Json(json!({
        "ok": true,
        "name": package.manifest.name,
        "variables": package.manifest.variables,
        "target_apps": package.manifest.target_apps,
    })))
}

#[derive(serde::Deserialize)]
struct SkillVerifyRequest {
    path: PathBuf,
}

async fn skill_verify(Json(request): Json<SkillVerifyRequest>) -> Json<Value> {
    match validate_skill_package(&request.path) {
        Ok(info) => Json(json!({
            "ok": true,
            "name": info.name,
            "permissions": info.permissions,
            "has_tests": info.has_tests,
        })),
        Err(error) => Json(json!({ "ok": false, "error": error })),
    }
}

#[derive(serde::Deserialize)]
struct ProactiveObserveRequest {
    app_id: String,
    actions: Vec<String>,
}

async fn proactive_observe(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ProactiveObserveRequest>,
) -> Result<Json<Option<ProactiveSuggestion>>, (StatusCode, String)> {
    let mut proactive = state.proactive.lock().map_err(poison)?;
    Ok(Json(proactive.observe(&request.app_id, request.actions)))
}

#[derive(serde::Deserialize)]
struct ProactiveDecideRequest {
    suggestion_id: String,
    action: SuggestionAction,
}

async fn proactive_decide(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ProactiveDecideRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut proactive = state.proactive.lock().map_err(poison)?;
    let suggestion = proactive
        .suggestions()
        .iter()
        .find(|suggestion| suggestion.id == request.suggestion_id)
        .cloned()
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("建议不存在：{}", request.suggestion_id),
            )
        })?;
    proactive
        .decide(&request.suggestion_id, request.action)
        .map_err(|error| (StatusCode::NOT_FOUND, error))?;
    drop(proactive);
    let mut response = json!({ "ok": true });
    if request.action == owo_agent_core::SuggestionAction::Learn {
        // 用户确认“学习”：把建议动作序列沉淀为 active 流程技能包（D24 一键学习）。
        let short_id: String = suggestion.id.chars().take(8).collect();
        let name = format!("proactive-{short_id}");
        let samples = owo_agent_core::recorded_actions_from_sequence(
            &suggestion.app_id,
            &suggestion.sequence,
        );
        let pipeline = state.pipeline.lock().map_err(poison)?;
        let package = pipeline
            .sink_from_actions(
                &name,
                vec![suggestion.app_id.clone()],
                owo_agent_core::Sensitivity::Low,
                &suggestion.summary,
                samples,
            )
            .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
        if let Ok(mut audit) = state.agent.audit_log().lock() {
            audit.record(
                "proactive",
                "learn-confirm",
                Some(package.manifest.name.clone()),
                Some(true),
                format!("主动建议确认沉淀技能包：{}", package.manifest.name),
            );
        }
        response["package"] = json!({
            "name": package.manifest.name,
            "variables": package.manifest.variables,
        });
    }
    Ok(Json(response))
}

/// 主动建议列表（桌面端“学习/执行一次/忽略/静默”四选）。
async fn proactive_suggestions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<ProactiveSuggestion>>, (StatusCode, String)> {
    let proactive = state.proactive.lock().map_err(poison)?;
    Ok(Json(proactive.suggestions().to_vec()))
}

/// 本地离线转写：请求体为 WAV 字节（16k PCM），返回文本（SenseVoice-Small）。
async fn stt_transcribe(
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, String)> {
    let wav_path = std::env::temp_dir().join(format!("owo-stt-{}.wav", uuid::Uuid::new_v4()));
    std::fs::write(&wav_path, &body)
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    let result = match state.stt.lock() {
        Ok(stt) => stt
            .transcribe_wav(&wav_path)
            .map(|outcome| (outcome, stt.engine().to_string())),
        Err(_) => Err("状态锁中毒".to_string()),
    };
    let _ = std::fs::remove_file(&wav_path);
    let (outcome, engine) = result.map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    Ok(Json(json!({
        "ok": true,
        "text": outcome.text,
        "elapsed_ms": outcome.elapsed_ms,
        "engine": engine,
    })))
}

// ---------- 自动化 ----------

#[derive(serde::Deserialize)]
struct CreateAutomationRequest {
    name: String,
    schedule: Schedule,
    reminder: String,
}

async fn automations_list(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<AutomationTask>>, (StatusCode, String)> {
    let automations = state.automations.lock().map_err(poison)?;
    Ok(Json(automations.list()))
}

async fn automations_create(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateAutomationRequest>,
) -> Result<Json<AutomationTask>, (StatusCode, String)> {
    let task = AutomationTask::new(
        &request.name,
        request.schedule,
        AutomationAction::Reminder {
            text: request.reminder,
        },
    );
    let mut automations = state.automations.lock().map_err(poison)?;
    automations
        .upsert(task.clone())
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    Ok(Json(task))
}

async fn automations_toggle(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut automations = state.automations.lock().map_err(poison)?;
    let enabled = automations
        .toggle(&id)
        .map_err(|error| (StatusCode::NOT_FOUND, error))?;
    Ok(Json(json!({ "id": id, "enabled": enabled })))
}

async fn automations_delete(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut automations = state.automations.lock().map_err(poison)?;
    automations
        .remove(&id)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    Ok(Json(json!({ "ok": true })))
}

async fn automations_reminders(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<String>>, (StatusCode, String)> {
    let automations = state.automations.lock().map_err(poison)?;
    Ok(Json(automations.reminders().to_vec()))
}

async fn automations_clear_reminders(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut automations = state.automations.lock().map_err(poison)?;
    automations
        .clear_reminders()
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::{
        auto_approve_enabled, rewind_session, sanitize_attachment_name, AppState, RewindRequest,
    };
    use async_trait::async_trait;
    use base64::Engine;
    use owo_agent_core::permissions::Policy;
    use owo_agent_core::{
        Agent, AgentConfig, ChatMessage, ModelOutput, ModelProvider, Session, ToolRegistry,
        ToolSpec,
    };
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn sanitizes_attachment_names() {
        assert_eq!(
            sanitize_attachment_name("report.pdf").as_deref(),
            Some("report.pdf")
        );
        assert_eq!(
            sanitize_attachment_name("a/b/c.txt").as_deref(),
            Some("c.txt")
        );
        assert_eq!(
            sanitize_attachment_name("..\\evil.txt").as_deref(),
            Some("evil.txt")
        );
        assert_eq!(
            sanitize_attachment_name("a:b*c?.txt").as_deref(),
            Some("bc.txt")
        );
        assert!(sanitize_attachment_name("").is_none());
        assert!(sanitize_attachment_name("   ").is_none());
        assert!(sanitize_attachment_name("x".repeat(201).as_str()).is_none());
    }

    #[test]
    fn auto_approve_env_detection() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::env::remove_var("OWO_AUTO_APPROVE");
        assert!(!auto_approve_enabled());
        std::env::set_var("OWO_AUTO_APPROVE", "1");
        assert!(auto_approve_enabled());
        std::env::set_var("OWO_AUTO_APPROVE", "TRUE");
        assert!(auto_approve_enabled());
        std::env::set_var("OWO_AUTO_APPROVE", "0");
        assert!(!auto_approve_enabled());
        std::env::remove_var("OWO_AUTO_APPROVE");
    }

    struct IdleProvider;

    #[async_trait]
    impl ModelProvider for IdleProvider {
        async fn complete(
            &self,
            _messages: &[ChatMessage],
            _tools: &[ToolSpec],
        ) -> Result<ModelOutput, String> {
            Err("测试 Provider 不应被调用".to_string())
        }
    }

    #[tokio::test]
    async fn rewind_endpoint_restores_files_before_saving_session() {
        let root = std::env::temp_dir().join(format!("owo-server-rewind-{}", uuid::Uuid::new_v4()));
        let workspace = root.join("workspace");
        let data_root = root.join("data");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&data_root).unwrap();
        let path = workspace.join("changed.txt");
        std::fs::write(&path, "after").unwrap();

        let agent = Agent::new(
            Arc::new(IdleProvider),
            ToolRegistry::new(),
            Policy::new(&workspace),
            AgentConfig::default(),
        );
        let store_root = root.join("sessions");
        let state = Arc::new(AppState::new(
            agent,
            owo_agent_core::JsonSessionStore::new(&store_root),
            data_root.join("traces"),
            data_root,
            workspace.clone(),
        ));
        let mut session = Session::new(&workspace, "mock", None);
        session.push(ChatMessage::user("first".to_string()));
        session.push(ChatMessage::assistant_text("reply".to_string()));
        session.snapshots.insert(
            path.to_string_lossy().replace('\\', "/"),
            owo_agent_core::session::SnapshotEntry {
                original_b64: Some(base64::engine::general_purpose::STANDARD.encode("before")),
            },
        );
        state.store.save(&session).unwrap();
        state
            .sessions
            .lock()
            .unwrap()
            .insert(session.id.clone(), session.clone());

        let id = session.id.clone();
        let response = rewind_session(
            axum::extract::State(Arc::clone(&state)),
            axum::extract::Path(id.clone()),
            axum::Json(RewindRequest { keep: 1 }),
        )
        .await
        .unwrap()
        .0;

        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["removed"], json!(1));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "before");
        assert_eq!(state.store.load(&id).unwrap().messages.len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }
}

/// 自动化常驻循环：每秒检查到期任务，触发提醒并写审计。
pub async fn start_automation_loop(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    loop {
        interval.tick().await;
        let fired = {
            let mut automations = state
                .automations
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let now = chrono::Utc::now();
            let mut fired = Vec::new();
            for id in automations.due_tasks(now) {
                if let Ok(text) = automations.fire(&id, now) {
                    fired.push(text);
                }
            }
            fired
        };
        if !fired.is_empty() {
            if let Ok(mut audit) = state.agent.audit_log().lock() {
                audit.record("automation", "fire", None, Some(true), fired.join(" | "));
            }
        }
    }
}

/// 静默观察器：每 2s 采样桌面状态（前台应用/标题哈希/剪贴板序列，受 L0 授权门控），
/// 并在模拟面下额外拉取模拟窗口日志；动作摘要（内容掩码）写入情景记忆。
pub async fn start_memory_observer(state: Arc<AppState>) {
    let mut sim_seen = 0usize;
    let mut desktop_prev: Option<owo_agent_core::DesktopSnapshot> = None;
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let l0_enabled = state
            .perception
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_enabled(owo_agent_core::PerceptionLayer::L0Event);
        if l0_enabled {
            let snapshot = owo_agent_core::sample_desktop();
            if let Some(prev) = &desktop_prev {
                if let Some(observation) = owo_agent_core::desktop_observation(prev, &snapshot) {
                    let mut memory = state
                        .memory
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let _ = memory.append(observation);
                }
            }
            desktop_prev = Some(snapshot);
        }
        let Some(base) = std::env::var("OWO_SIM_QQ_URL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let url = format!("{}/log", base.trim_end_matches('/'));
        let Ok(response) = reqwest::get(&url).await else {
            continue;
        };
        let Ok(value) = response.json::<Value>().await else {
            continue;
        };
        let Some(entries) = value.get("entries").and_then(Value::as_array) else {
            continue;
        };
        if entries.len() < sim_seen {
            // 模拟场景被 /reset 清空：从头重新计数。
            sim_seen = 0;
        }
        if entries.len() <= sim_seen {
            continue;
        }
        let mut memory = state
            .memory
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for entry in &entries[sim_seen..] {
            if let Some(observation) = owo_agent_core::observation_from_sim_event(entry) {
                let _ = memory.append(observation);
            }
        }
        sim_seen = entries.len();
    }
}

// ---------- 设置与诊断 ----------

async fn settings_get(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let settings = owo_agent_core::Settings::load(&state.workspace);
    serde_json::to_value(&settings)
        .map(Json)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
}

#[derive(serde::Deserialize)]
struct EgressRequest {
    cloud_enabled: bool,
}

async fn settings_egress(
    State(state): State<Arc<AppState>>,
    Json(request): Json<EgressRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut settings = owo_agent_core::Settings::load(&state.workspace);
    settings.egress.cloud_enabled = request.cloud_enabled;
    settings
        .save(&state.workspace)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    std::env::set_var(
        "OWO_CLOUD_ENABLED",
        if request.cloud_enabled {
            "true"
        } else {
            "false"
        },
    );
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            "settings",
            "egress",
            None,
            Some(request.cloud_enabled),
            format!("数据出境开关：cloud_enabled={}", request.cloud_enabled),
        );
    }
    Ok(Json(json!({
        "cloud_enabled": request.cloud_enabled,
        "note": "已写入 settings.json 并即时生效",
    })))
}

/// 通用设置保存：写入 settings.json 并应用运行时设置（数据出境、STT、主动建议、白名单）。
async fn settings_update(
    State(state): State<Arc<AppState>>,
    Json(settings): Json<owo_agent_core::Settings>,
) -> Result<Json<Value>, (StatusCode, String)> {
    settings
        .save(&state.workspace)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    settings.apply_usage_env();
    state
        .agent
        .apply_policy_settings(settings.read_only, &settings.deny_commands);
    if let Some(model) = &settings.model {
        if !model.trim().is_empty() {
            std::env::set_var("OPENAI_MODEL", model);
        }
    }
    std::env::set_var(
        "OWO_CLOUD_ENABLED",
        settings.egress.cloud_enabled.to_string(),
    );
    if let Ok(mut stt) = state.stt.lock() {
        stt.apply_settings(&settings.stt);
    }
    if let Ok(mut proactive) = state.proactive.lock() {
        proactive.apply_settings(settings.proactive.clone());
    }
    if let Ok(mut whitelist) = state.whitelist.lock() {
        let mut merged = Whitelist::default();
        for entry in settings.whitelist.clone() {
            merged.upsert(entry);
        }
        *whitelist = merged;
    }
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            "settings",
            "update",
            None,
            Some(true),
            "设置页保存（settings.json）",
        );
    }
    Ok(Json(json!({
        "ok": true,
        "note": "已写入 settings.json 并应用运行时设置（模型对新回合即时生效）",
    })))
}

async fn whitelist_list(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<WhitelistEntry>>, (StatusCode, String)> {
    let whitelist = state.whitelist.lock().map_err(poison)?;
    Ok(Json(whitelist.entries().to_vec()))
}

#[derive(serde::Deserialize)]
struct WhitelistManageRequest {
    action: String,
    #[serde(default)]
    entry: Option<WhitelistEntry>,
    #[serde(default)]
    app_id: Option<String>,
}

async fn whitelist_manage(
    State(state): State<Arc<AppState>>,
    Json(request): Json<WhitelistManageRequest>,
) -> Result<Json<Vec<WhitelistEntry>>, (StatusCode, String)> {
    let action = request.action.clone();
    let entry = request.entry.clone();
    let app_id = request.app_id.clone();
    let entries = {
        let mut whitelist = state.whitelist.lock().map_err(poison)?;
        match action.as_str() {
            "upsert" => {
                let entry = entry
                    .clone()
                    .ok_or((StatusCode::BAD_REQUEST, "upsert 需要 entry".to_string()))?;
                whitelist.upsert(entry);
            }
            "remove" => {
                let app_id = app_id
                    .clone()
                    .ok_or((StatusCode::BAD_REQUEST, "remove 需要 app_id".to_string()))?;
                whitelist.remove(&app_id);
            }
            other => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!("未知操作：{other}（upsert / remove）"),
                ))
            }
        }
        whitelist.entries().to_vec()
    };
    let mut settings = owo_agent_core::Settings::load(&state.workspace);
    match action.as_str() {
        "upsert" => {
            let entry = entry.ok_or((StatusCode::BAD_REQUEST, "upsert 需要 entry".to_string()))?;
            if let Some(existing) = settings
                .whitelist
                .iter_mut()
                .find(|existing| existing.app_id == entry.app_id)
            {
                *existing = entry.clone();
            } else {
                settings.whitelist.push(entry);
            }
        }
        "remove" => {
            let app_id =
                app_id.ok_or((StatusCode::BAD_REQUEST, "remove 需要 app_id".to_string()))?;
            settings
                .whitelist
                .retain(|existing| existing.app_id != app_id);
        }
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("未知操作：{other}（upsert / remove）"),
            ))
        }
    }
    settings
        .save(&state.workspace)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    Ok(Json(entries))
}

struct ChannelApprover {
    pending: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<Decision>>>>,
    pending_sessions: Arc<Mutex<HashMap<String, String>>>,
    session_id: String,
    abort: Arc<AtomicBool>,
}

impl ChannelApprover {
    fn spawn_request(
        &self,
        request: &PermissionRequest,
    ) -> tokio::sync::oneshot::Receiver<Decision> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(request.request_id.clone(), tx);
        }
        if let Ok(mut sessions) = self.pending_sessions.lock() {
            sessions.insert(request.request_id.clone(), self.session_id.clone());
        }
        rx
    }
}

#[async_trait::async_trait]
impl Approver for ChannelApprover {
    async fn decide(&self, request: &PermissionRequest) -> Decision {
        if auto_approve_enabled() {
            return Decision::Allow;
        }
        let rx = self.spawn_request(request);
        let mut rx = rx;
        let deadline = tokio::time::sleep(std::time::Duration::from_secs(300));
        tokio::pin!(deadline);
        let decision = loop {
            tokio::select! {
                result = &mut rx => break result.unwrap_or(Decision::Deny),
                _ = &mut deadline => break Decision::Deny,
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
                    if self.abort.load(Ordering::Relaxed) {
                        break Decision::Deny;
                    }
                }
            }
        };
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&request.request_id);
        }
        if let Ok(mut sessions) = self.pending_sessions.lock() {
            sessions.remove(&request.request_id);
        }
        decision
    }
}

fn auto_approve_enabled() -> bool {
    std::env::var("OWO_AUTO_APPROVE")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn to_sse(event: &owo_agent_core::TurnEvent) -> Option<SseEvent> {
    match event {
        owo_agent_core::TurnEvent::ModelCall => Some(SseEvent::Progress {
            message: "模型调用".to_string(),
        }),
        owo_agent_core::TurnEvent::TokenDelta { delta } => Some(SseEvent::TokenDelta {
            delta: delta.clone(),
        }),
        owo_agent_core::TurnEvent::Compaction { summary } => Some(SseEvent::Compaction {
            summary: summary.clone(),
        }),
        owo_agent_core::TurnEvent::PermissionRequest(request) => {
            Some(SseEvent::PermissionRequest {
                request_id: request.request_id.clone(),
                tool: request.tool.clone(),
                args: request.args.clone(),
                reason: request.reason.clone(),
            })
        }
        owo_agent_core::TurnEvent::ToolStart { id, tool } => Some(SseEvent::ToolUse {
            id: id.clone(),
            tool: tool.clone(),
            args: Value::Null,
        }),
        owo_agent_core::TurnEvent::ToolResult {
            id,
            tool,
            ok,
            error,
        } => Some(SseEvent::ToolResult {
            id: id.clone(),
            tool: tool.clone(),
            ok: *ok,
            error: error.clone(),
        }),
        owo_agent_core::TurnEvent::Final { text } => Some(SseEvent::Final { text: text.clone() }),
    }
}

fn poison<T>(_error: std::sync::PoisonError<T>) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, "状态锁中毒".to_string())
}

fn require_perception_layer(
    state: &AppState,
    layer: owo_agent_core::PerceptionLayer,
) -> Result<(), (StatusCode, String)> {
    let perception = state.perception.lock().map_err(poison)?;
    if perception.is_enabled(layer) {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, format!("感知层未授权：{layer:?}")))
    }
}

/// P3 录制自动观察：录制中每 2s 采样前台应用/剪贴板事件（掩码）进入样本。
/// 前台应用变化只记一次，剪贴板变化按序列号去重。
pub async fn start_observer(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    let mut last_app: Option<(String, String)> = None;
    let mut last_clipboard: u32 = 0;
    loop {
        interval.tick().await;
        let (foreground, clipboard_changed) = {
            let mut perception = state
                .perception
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let _ = perception.refresh_from_platform();
            let sequence = owo_agent_core::clipboard_sequence();
            let changed = sequence != 0 && sequence != last_clipboard;
            perception.refresh_clipboard(sequence);
            let _ = perception.refresh_from_uia(2, 32);
            let snapshot = perception.snapshot();
            (snapshot.foreground_app.clone(), changed)
        };
        let mut pipeline = state
            .pipeline
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if pipeline.recorder.state() != LearnState::Recording {
            continue;
        }
        if let Some(app) = &foreground {
            let key = (app.id.clone(), app.title.clone());
            if last_app.as_ref() != Some(&key) {
                last_app = Some(key);
                let _ = pipeline.recorder.record(RecordedAction {
                    app_id: app.id.clone(),
                    anchor: SemanticAnchor {
                        app_id: Some(app.id.clone()),
                        role: None,
                        name: app.title.clone(),
                        parent: None,
                        element_id: None,
                    },
                    action_type: ActionType::Shortcut,
                    value_masked: true,
                    sensitive: false,
                    at: chrono::Utc::now().to_rfc3339(),
                });
            }
        }
        if clipboard_changed {
            last_clipboard = owo_agent_core::clipboard_sequence();
            if let Some(app) = &foreground {
                let _ = pipeline.recorder.record(RecordedAction {
                    app_id: app.id.clone(),
                    anchor: SemanticAnchor {
                        app_id: Some(app.id.clone()),
                        role: None,
                        name: "剪贴板".to_string(),
                        parent: None,
                        element_id: None,
                    },
                    action_type: ActionType::Inject,
                    value_masked: true,
                    sensitive: false,
                    at: chrono::Utc::now().to_rfc3339(),
                });
            }
        }
    }
}

// ---------- v0.5 恢复：M-A 定位 / M-C 记忆 / M-D 技能健康 / 插件 / 子代理 / MCP / Traces ----------
// struct PluginEnabledRequest (行 636-638)
#[derive(serde::Deserialize)]
struct PluginEnabledRequest {
    enabled: bool,
}

// struct SubagentRunRequest (行 731-737)
#[derive(serde::Deserialize)]
struct SubagentRunRequest {
    prompt: String,
    /// true 为只读探索模式（对齐 CLI `@explore`）；false 为通用子代理（对齐 `@subagent`）。
    #[serde(default)]
    read_only: bool,
    #[serde(default)]
    model: Option<String>,
}

// struct McpAddRequest (行 920-933)
#[derive(serde::Deserialize)]
struct McpAddRequest {
    name: String,
    /// "stdio" 或 "http"
    #[serde(default = "default_mcp_transport")]
    transport: String,
    /// stdio 传输时的启动命令
    #[serde(default)]
    command: String,
    #[serde(default)]
    args: Vec<String>,
    /// http 传输时的端点 URL
    #[serde(default)]
    url: Option<String>,
}

fn default_mcp_transport() -> String {
    "stdio".to_string()
}

// struct McpRemoveRequest (行 990-992)
#[derive(serde::Deserialize)]
struct McpRemoveRequest {
    name: String,
}

fn load_mcp_configs(root: &Path) -> Vec<owo_agent_core::McpServerConfig> {
    std::fs::read_to_string(root.join("mcp-servers.json"))
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

fn save_mcp_configs(root: &Path, configs: &[owo_agent_core::McpServerConfig]) {
    if let Ok(content) = serde_json::to_string_pretty(configs) {
        let _ = std::fs::write(root.join("mcp-servers.json"), content);
    }
}

// ==== session_context (备份行 1332-1365) ====
async fn session_context(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let session = load_session(&state, &id)?;
    let messages: Vec<owo_agent_core::ChatMessage> = session.messages.clone();
    let estimated = owo_agent_core::estimate_tokens(&messages);
    let rules = owo_agent_core::context::load_project_rules(&session.workspace);
    let config = state.agent.config();
    let mut last_compaction: Option<String> = None;
    // 反向找最近的压缩摘要（system 消息以"历史摘要"开头）。
    for message in messages.iter().rev() {
        if message.role == "system"
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.starts_with("历史摘要"))
        {
            last_compaction = message.content.clone();
            break;
        }
    }
    Ok(Json(json!({
        "session_id": id,
        "messages": messages.len(),
        "estimated_tokens": estimated,
        "token_budget": config.token_budget,
        "compaction_enabled": config.compaction_enabled,
        "over_budget": estimated > config.token_budget,
        "rules_injected": !rules.is_empty(),
        "rules_chars": rules.chars().count(),
        "last_compaction": last_compaction,
    })))
}

// ==== skills_health (备份行 1111-1133) ====
async fn skills_health(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pipeline = state.pipeline.lock().map_err(poison)?;
    let skills: Vec<Value> = pipeline
        .store
        .list_health()
        .into_iter()
        .map(|(name, health)| {
            json!({
                "name": name,
                "state": health.state,
                "attempts": health.attempts,
                "successes": health.successes,
                "success_rate": health.success_rate(),
                "consecutive_failures": health.consecutive_failures,
                "template_hit_rate": health.template_hit_rate(),
                "recent_failures": health.recent_failures,
            })
        })
        .collect();
    Ok(Json(json!({ "count": skills.len(), "skills": skills })))
}

// ==== skill_health_reset (备份行 1136-1153) ====
async fn skill_health_reset(
    State(state): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pipeline = state.pipeline.lock().map_err(poison)?;
    pipeline
        .store
        .reset_health(&name)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            "learn",
            "health-reset",
            Some(name.clone()),
            Some(true),
            format!("重置技能健康度：{name}"),
        );
    }
    Ok(Json(json!({ "ok": true, "name": name })))
}

// ==== plugins_list (备份行 600-633) ====
async fn plugins_list(State(state): State<Arc<AppState>>) -> Json<Value> {
    let plugins = owo_agent_core::discover_plugins(&state.workspace, &state.data_root);
    let plugin_state = state
        .plugin_state
        .lock()
        .map(|guard| guard.disabled_ids())
        .unwrap_or_default();
    let items: Vec<Value> = plugins
        .into_iter()
        .map(|(path, manifest)| {
            let enabled = !plugin_state.contains(&manifest.id);
            let tools_hidden = state
                .agent
                .tool_disabled(&owo_agent_core::tools::mcp_tool_prefix(&manifest.id));
            json!({
                "id": manifest.id,
                "name": manifest.name,
                "version": manifest.version,
                "description": manifest.description,
                "enabled": enabled,
                "tools_hidden": tools_hidden,
                "permissions": manifest.permissions,
                "mcp": manifest.mcp.as_ref().map(|mcp| json!({
                    "name": mcp.name,
                    "transport": mcp.transport,
                    "command": mcp.command,
                    "args": mcp.args,
                })),
                "manifest_path": path.to_string_lossy(),
            })
        })
        .collect();
    Json(json!({ "count": items.len(), "plugins": items }))
}

// ==== plugin_enabled (备份行 642-726) ====
async fn plugin_enabled(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<PluginEnabledRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    {
        let mut plugin_state = state.plugin_state.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "插件状态锁中毒".to_string(),
            )
        })?;
        plugin_state
            .set_enabled(&id, request.enabled)
            .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    }
    let prefix = owo_agent_core::tools::mcp_tool_prefix(&id);
    let mut process_killed = false;
    let mut tools = 0usize;
    if request.enabled {
        // 启用：重新连接插件 MCP 服务器并注册工具（幂等：先清理旧连接再连接）。
        let _ = state.agent.shutdown_mcp_server(&id).await;
        let discovered = owo_agent_core::discover_plugins(&state.workspace, &state.data_root);
        let Some((manifest_path, manifest)) = discovered.into_iter().find(|(_, m)| m.id == id)
        else {
            return Err((
                StatusCode::NOT_FOUND,
                format!("插件 {id} 不存在（已从工作区移除？）"),
            ));
        };
        match owo_agent_core::plugin_mcp_config(&manifest_path, &manifest) {
            Some(config) => match state.agent.connect_mcp_server(&config).await {
                Ok(count) => {
                    tools = count;
                    state.agent.set_tool_prefix_enabled(&prefix, true);
                }
                Err(error) => {
                    // 连接失败仍标记启用（状态持久化），工具不可用由 UI 提示。
                    return Err((
                        StatusCode::BAD_GATEWAY,
                        format!("插件 MCP 服务器连接失败：{error}"),
                    ));
                }
            },
            None => {
                // 无 MCP 声明（纯视图插件）：仅恢复前缀。
                state.agent.set_tool_prefix_enabled(&prefix, true);
            }
        }
    } else {
        // 禁用：进程级热卸载（kill 子进程 + 撤销工具）。
        process_killed = state
            .agent
            .shutdown_mcp_server(&id)
            .await
            .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    }
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            "plugin",
            "set-enabled",
            Some(id.clone()),
            Some(true),
            format!(
                "插件 {} 已{}（{}）",
                id,
                if request.enabled { "启用" } else { "禁用" },
                if request.enabled {
                    format!("重新连接 MCP，工具 {tools} 个")
                } else if process_killed {
                    "进程级热卸载（子进程已终止）".to_string()
                } else {
                    "无 MCP 子进程".to_string()
                }
            ),
        );
    }
    Ok(Json(json!({
        "ok": true,
        "id": id,
        "enabled": request.enabled,
        "process_killed": process_killed,
        "tools": tools,
    })))
}

// ==== subagent_run (备份行 742-788) ====
async fn subagent_run(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SubagentRunRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if request.prompt.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "缺少 prompt".to_string()));
    }
    let started = std::time::Instant::now();
    let workspace = state.workspace.clone();
    let model = request
        .model
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".to_string())
        });
    let agent = Arc::clone(&state.agent);
    let text = agent
        .run_subagent(&workspace, &model, &request.prompt, request.read_only)
        .await
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let duration_ms = started.elapsed().as_millis() as u64;
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            "subagent",
            if request.read_only { "explore" } else { "run" },
            None,
            Some(true),
            format!(
                "{}子代理完成（{}ms）：{}",
                if request.read_only {
                    "只读探索"
                } else {
                    "通用"
                },
                duration_ms,
                request.prompt.chars().take(120).collect::<String>()
            ),
        );
    }
    Ok(Json(json!({
        "ok": true,
        "read_only": request.read_only,
        "model": model,
        "duration_ms": duration_ms,
        "text": text,
    })))
}

// ==== mcp_list (备份行 904-917) ====
async fn mcp_list(State(state): State<Arc<AppState>>) -> Json<Value> {
    let configs = load_mcp_configs(&state.data_root);
    let settings = owo_agent_core::Settings::load(&state.workspace);
    let mut merged = configs.clone();
    for server in settings.mcp_servers {
        if !merged.iter().any(|config| config.name == server.name) {
            merged.push(server);
        }
    }
    Json(json!({
        "count": merged.len(),
        "servers": merged,
    }))
}

// ==== mcp_add (备份行 940-987) ====
async fn mcp_add(
    State(state): State<Arc<AppState>>,
    Json(request): Json<McpAddRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let name = request.name.trim().to_string();
    if name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "缺少名称 name".to_string()));
    }
    let config = owo_agent_core::McpServerConfig {
        name: name.clone(),
        transport: request.transport.clone(),
        command: request.command.clone(),
        args: request.args.clone(),
        url: request.url.clone(),
        timeout_ms: None,
        network_allowlist: Vec::new(),
    };
    let mut configs = load_mcp_configs(&state.data_root);
    if configs.iter().any(|existing| existing.name == name) {
        return Err((StatusCode::CONFLICT, format!("MCP 服务器 {name} 已存在")));
    }
    // 热连接（经 Agent 注册表：注册工具 + 记入进程注册表，可被 /mcp/remove 进程级卸载）。
    let tool_count = state
        .agent
        .connect_mcp_server(&config)
        .await
        .map_err(|error| (StatusCode::BAD_GATEWAY, format!("连接失败：{error}")))?;
    let connected = true;
    configs.push(config);
    save_mcp_configs(&state.data_root, &configs);
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            "mcp",
            "add",
            Some(name.clone()),
            Some(true),
            format!(
                "新增 MCP 服务器 {name}（{}，工具 {tool_count} 个）",
                request.transport
            ),
        );
    }
    Ok(Json(json!({
        "ok": true,
        "name": name,
        "connected": connected,
        "tools": tool_count,
    })))
}

// ==== mcp_remove (备份行 995-1028) ====
async fn mcp_remove(
    State(state): State<Arc<AppState>>,
    Json(request): Json<McpRemoveRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let name = request.name.trim().to_string();
    let mut configs = load_mcp_configs(&state.data_root);
    let before = configs.len();
    configs.retain(|config| config.name != name);
    if configs.len() == before {
        return Err((StatusCode::NOT_FOUND, format!("MCP 服务器 {name} 不存在")));
    }
    save_mcp_configs(&state.data_root, &configs);
    //
    // 进程级卸载：kill stdio 子进程 + 撤销工具（前缀移除且禁用）。
    let process_killed = state
        .agent
        .shutdown_mcp_server(&name)
        .await
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            "mcp",
            "remove",
            Some(name.clone()),
            Some(true),
            format!(
                "移除 MCP 服务器 {name}（{}）",
                if process_killed {
                    "子进程已终止"
                } else {
                    "未连接"
                }
            ),
        );
    }
    Ok(Json(
        json!({ "ok": true, "name": name, "process_killed": process_killed }),
    ))
}

// ==== locate_query (备份行 1049-1108) ====
async fn locate_query(
    State(state): State<Arc<AppState>>,
    Json(query): Json<AnchorQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let elements = state.elements.lock().map_err(poison)?;
    let elements: Vec<SceneElement> = if query.app_id.is_some() {
        elements.list(query.app_id.as_deref().unwrap_or_default())
    } else {
        elements.list_all()
    };
    let mut graph_elements: Vec<GraphElement> = elements
        .iter()
        .map(|element| {
            let mut entry = GraphElement::from_element(element.clone());
            let source = if element.sources.contains(&"uia".to_string()) {
                EvidenceSource::Uia
            } else if element.sources.contains(&"ocr".to_string()) {
                EvidenceSource::Ocr
            } else {
                EvidenceSource::Vision
            };
            entry.add_evidence(Evidence::new(source, element, element.confidence));
            entry
        })
        .collect();
    let mut graph = state.scene.lock().map_err(poison)?;
    graph.update(None, None, std::mem::take(&mut graph_elements));

    let result = locate(&graph, &query);
    if let Some(best) = &result.best {
        graph.record_hit(&best.id, &query.signature());
    }
    let candidates: Vec<Value> = result
        .candidates
        .iter()
        .map(|(element, score)| {
            json!({
                "id": element.id,
                "name": element.name,
                "role": element.role_hint,
                "rect": [element.x, element.y, element.width, element.height],
                "score": score,
            })
        })
        .collect();
    Ok(Json(json!({
        "count": candidates.len(),
        "candidates": candidates,
        "best": result.best.as_ref().map(|element| json!({
            "id": element.id,
            "name": element.name,
            "role": element.role_hint,
            "rect": [element.x, element.y, element.width, element.height],
            "confidence": element.confidence,
        })),
        "uncertainty": result.uncertainty,
        "used_source": result.used_source.map(|source| format!("{source:?}").to_lowercase()),
        "reliable": result.is_reliable(),
    })))
}

// ==== traces_list (备份行 1840-1871) ====
async fn traces_list(State(state): State<Arc<AppState>>) -> Json<Value> {
    let traces = owo_agent_core::list_traces(&state.traces_dir);
    let items: Vec<Value> = traces
        .iter()
        .filter_map(|path| {
            let trace = owo_agent_core::load_trace(path).ok()?;
            let preview: String = trace.prompt.chars().take(60).collect();
            Some(json!({
                        "index": {
                            //
            // index 为在倒序列表中的位置（回放用）。
                            "position": traces.iter().position(|p| p == path).unwrap_or(0),
                        },
                        "file": path.file_name().unwrap_or_default().to_string_lossy(),
                        "session_id": trace.session_id,
                        "model": trace.model,
                        "prompt_preview": preview,
                        "prompt": trace.prompt,
                        "started_at": trace.started_at,
                        "duration_ms": trace.duration_ms,
                        "steps": trace.steps,
                        "has_final": trace.final_text.is_some(),
                        "final_text": trace.final_text,
                        "usage": json!({
                            "prompt_tokens": trace.usage.prompt_tokens,
                            "completion_tokens": trace.usage.completion_tokens,
                            "total_tokens": trace.usage.total_tokens,
                        }),
                    }))
        })
        .collect();
    Json(json!({ "count": items.len(), "traces": items }))
}

// ==== trace_show (备份行 1874-1901) ====
async fn trace_show(
    State(state): State<Arc<AppState>>,
    AxumPath(index): AxumPath<usize>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let traces = owo_agent_core::list_traces(&state.traces_dir);
    let path = traces.get(index).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("trace 序号越界（共 {} 条）", traces.len()),
        )
    })?;
    let trace = owo_agent_core::load_trace(path)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(json!({
        "index": index,
        "file": path.file_name().unwrap_or_default().to_string_lossy(),
        "session_id": trace.session_id,
        "workspace": trace.workspace,
        "model": trace.model,
        "prompt": trace.prompt,
        "started_at": trace.started_at,
        "duration_ms": trace.duration_ms,
        "steps": trace.steps,
        "final_text": trace.final_text,
        "usage": trace.usage,
        "events": trace.events,
    })))
}

// ==== memory_recall (备份行 2611-2631) ====
async fn memory_recall(
    State(state): State<Arc<AppState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let q = params
        .get("q")
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "缺少查询参数 q".to_string()))?;
    let top_k = params
        .get("top_k")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(5)
        .min(50);
    let memory = state.memory.lock().map_err(poison)?;
    let hits = memory.recall(q, top_k);
    Ok(Json(json!({
        "count": hits.len(),
        "hits": hits,
    })))
}

// ==== project_rules_get ====
/// 项目规则列表：`GET /project/rules`。
/// 读取工作区 AGENTS.md / CLAUDE.md 并报告注入状态（会话启动时是否会加载）。
async fn project_rules_get(State(state): State<Arc<AppState>>) -> Json<Value> {
    let names = ["AGENTS.md", "CLAUDE.md"];
    let rules: Vec<Value> = names
        .iter()
        .map(|name| {
            let path = state.workspace.join(name);
            let exists = path.is_file();
            let content = if exists {
                std::fs::read_to_string(&path).unwrap_or_default()
            } else {
                String::new()
            };
            json!({
                "name": name,
                "path": path.to_string_lossy(),
                "exists": exists,
                "injected": exists,
                "content": content,
            })
        })
        .collect();
    Json(json!({
        "workspace": state.workspace.to_string_lossy(),
        "count": rules.len(),
        "rules": rules,
    }))
}

#[derive(serde::Deserialize)]
struct ProjectRulesRequest {
    content: String,
}

// ==== project_rules_post ====
async fn project_rules_post(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ProjectRulesRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let path = state.workspace.join("AGENTS.md");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    }
    std::fs::write(&path, &request.content)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            "project",
            "rules-write",
            Some("AGENTS.md".to_string()),
            Some(true),
            format!(
                "写入项目规则 AGENTS.md（{} 字符）",
                request.content.chars().count()
            ),
        );
    }
    Ok(Json(json!({
        "ok": true,
        "path": path.to_string_lossy(),
        "chars": request.content.chars().count(),
    })))
}

// ==== project_rules_template ====
async fn project_rules_template(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    const TEMPLATE: &str = "# AGENTS.md

<!-- 由 OwO Agent 生成，按项目实际情况修改。
     该文件会被 Agent 在每次会话开始时注入，作为项目级规则。 -->

## 项目说明

- 一句话描述本项目做什么。

## 开发规则

- 写清楚构建命令、测试命令与代码约定。
- 说明哪些目录/文件禁止修改。
";
    let path = state.workspace.join("AGENTS.md");
    if path.exists() {
        return Err((
            StatusCode::CONFLICT,
            format!("AGENTS.md 已存在（{}），未覆盖", path.display()),
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    }
    std::fs::write(&path, TEMPLATE)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            "project",
            "rules-template",
            Some("AGENTS.md".to_string()),
            Some(true),
            "生成 AGENTS.md 模板：init 等价操作".to_string(),
        );
    }
    Ok(Json(json!({
        "ok": true,
        "path": path.to_string_lossy(),
        "chars": TEMPLATE.chars().count(),
    })))
}

// ---------- computer-use 任务级审批（m4d 前奏，文档 7.3） ----------
/// 任务列表：`GET /computer-use/tasks`。
async fn computer_tasks_list(State(state): State<Arc<AppState>>) -> Json<Value> {
    let tasks = state.computer_tasks.list();
    Json(json!({ "count": tasks.len(), "tasks": tasks }))
}

#[derive(serde::Deserialize)]
struct ComputerTaskCreateRequest {
    target_app: String,
    description: String,
    #[serde(default)]
    allowed_actions: Vec<String>,
    #[serde(default = "default_task_duration_ms")]
    max_duration_ms: u64,
}

fn default_task_duration_ms() -> u64 {
    300_000
}

/// 创建任务：`POST /computer-use/task`（Pending，等待审批）。
async fn computer_task_create(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ComputerTaskCreateRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let target_app = request.target_app.trim().to_string();
    if target_app.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "缺少 target_app".to_string()));
    }
    let task = owo_agent_core::ComputerTask {
        id: uuid::Uuid::new_v4().to_string(),
        target_app,
        description: request.description,
        allowed_actions: request.allowed_actions,
        max_duration_ms: request.max_duration_ms,
        state: owo_agent_core::TaskState::Pending,
        created_at: chrono::Utc::now().to_rfc3339(),
        fuse_reason: None,
    };
    state
        .computer_tasks
        .create(task.clone())
        .map_err(|error| (StatusCode::CONFLICT, error))?;
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            "computer-use",
            "task-create",
            Some(task.id.clone()),
            Some(true),
            format!(
                "创建 computer-use 任务：{}（{}ms，动作 {:?}）",
                task.target_app, task.max_duration_ms, task.allowed_actions
            ),
        );
    }
    Ok(Json(json!({ "ok": true, "task": task })))
}

/// 状态迁移：`POST /computer-use/task/{id}/{action}`。
///
/// action ∈ approve/reject/cancel/start/pause/fuse/resume/complete。
async fn computer_task_transition(
    State(state): State<Arc<AppState>>,
    AxumPath((id, action)): AxumPath<(String, String)>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let reason = payload
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("人工接管")
        .to_string();
    let next = match action.as_str() {
        "approve" => state.computer_tasks.approve(&id),
        "reject" => state.computer_tasks.reject(&id),
        "cancel" => state.computer_tasks.cancel(&id),
        "start" => state.computer_tasks.start(&id),
        "pause" => state.computer_tasks.pause(&id, &reason),
        "fuse" => state.computer_tasks.fuse(&id, &reason),
        "resume" => state.computer_tasks.resume(&id),
        "complete" => state.computer_tasks.complete(&id),
        other => Err(format!(
            "未知动作：{other}（approve/reject/cancel/start/pause/fuse/resume/complete）"
        )),
    }
    .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    if let Ok(mut audit) = state.agent.audit_log().lock() {
        audit.record(
            "computer-use",
            &format!("task-{action}"),
            Some(id.clone()),
            Some(true),
            format!("computer-use 任务 {id} {action} → {:?}", next),
        );
    }
    Ok(Json(
        json!({ "ok": true, "id": id, "action": action, "state": format!("{next:?}") }),
    ))
}

/// 执行前检查：`GET /computer-use/task/{id}/check/{action}`（状态 + 超时 + 动作白名单）。
async fn computer_task_check(
    State(state): State<Arc<AppState>>,
    AxumPath((id, action)): AxumPath<(String, String)>,
) -> Result<Json<Value>, (StatusCode, String)> {
    state
        .computer_tasks
        .check_can_execute(&id)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    state
        .computer_tasks
        .check_action_allowed(&id, &action)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    let task = state
        .computer_tasks
        .get(&id)
        .ok_or((StatusCode::NOT_FOUND, format!("任务 {id} 不存在")))?;
    Ok(Json(json!({
        "ok": true,
        "id": id,
        "action": action,
        "state": format!("{:?}", task.state),
        "target_app": task.target_app,
    })))
}

#[derive(serde::Deserialize)]
struct SensitiveCheckRequest {
    name: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    ocr_text: String,
}

/// 敏感 UI 检测：`POST /computer-use/sensitive-check`（熔断判断，纯函数）。
async fn computer_sensitive_check(Json(request): Json<SensitiveCheckRequest>) -> Json<Value> {
    match owo_agent_core::sensitive_ui_hit(&request.name, &request.role, &request.ocr_text) {
        Some(reason) => Json(json!({ "sensitive": true, "reason": reason })),
        None => Json(json!({ "sensitive": false })),
    }
}

// ---------- computer-use 审批版闭环执行（M4d，HTTP 接入） ----------

#[derive(serde::Deserialize)]
struct ComputerTaskRunRequest {
    /// 闭环步骤（anchor_text/action/value/verify_text）。
    goals: Vec<owo_agent_core::computer_use::TaskGoal>,
}

/// 执行已批准任务：`POST /computer-use/task/{id}/run`。
///
/// 感知→定位→门禁动作→验证 全闭环；模拟面（OWO_SIM_QQ_URL）走 owo-sim-qq，
/// 否则走真实桌面面（RealTaskSurface）。任务未批准/越界应用/敏感熔断等门禁
/// 失败返回 403 并写审计；每步动作均过 `task_gate_check`。
async fn computer_task_run(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<ComputerTaskRunRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let task = state.computer_tasks.get(&id).ok_or((
        StatusCode::NOT_FOUND,
        format!("computer-use 任务 {id} 不存在"),
    ))?;
    if !task.state.can_execute() {
        return Err((
            StatusCode::FORBIDDEN,
            format!(
                "computer-use 任务 {id} 状态 {:?} 不可执行（需先 approve/start）",
                task.state
            ),
        ));
    }
    // 用本地 scratch 审计跑闭环（std MutexGuard 不能跨 await），完成后合并回真实审计。
    let mut scratch = owo_agent_core::audit::AuditLog::default();
    let report = if owo_agent_core::computer_use::sim_base_url_configured() {
        owo_agent_core::computer_use::run_approved_task(
            &state.computer_tasks,
            &mut scratch,
            "computer-use",
            &id,
            &request.goals,
        )
        .await
    } else {
        let mut surface = owo_agent_core::computer_use::RealTaskSurface;
        owo_agent_core::computer_use::run_approved_task_on(
            &state.computer_tasks,
            &mut scratch,
            "computer-use",
            &id,
            &request.goals,
            &mut surface,
        )
        .await
    };
    if let Ok(mut log) = state.agent.audit_log().lock() {
        log.entries.extend(scratch.entries);
    }
    report
        .map(|r| Json(json!(r)))
        .map_err(|error| (StatusCode::FORBIDDEN, error))
}

// ---------- 云端执行（M4a，/cloud/*） ----------

/// 懒初始化云端任务队列：传输按环境变量选择
/// （OWO_CLOUD_BASE_URL → HttpTransport；缺省 MockRemoteTransport 本地模拟）。
async fn cloud_queue(
    state: &AppState,
) -> Result<tokio::sync::MutexGuard<'_, Option<owo_agent_core::cloud_exec::CloudTaskQueue>>, String>
{
    let mut guard = state.cloud_queue.lock().await;
    if guard.is_none() {
        let dir = state.data_root.join("cloud").join("queue");
        std::fs::create_dir_all(&dir).map_err(|e| format!("创建云端队列目录失败：{e}"))?;
        let transport: Box<dyn owo_agent_core::cloud_exec::CloudTransport> =
            match std::env::var("OWO_CLOUD_BASE_URL") {
                Ok(url) if !url.trim().is_empty() => Box::new(
                    owo_agent_core::cloud_exec::HttpTransport::new(url)
                        .map_err(|e| format!("云端传输初始化失败：{e}"))?,
                ),
                _ => Box::new(owo_agent_core::cloud_exec::MockRemoteTransport::new(
                    state.data_root.join("cloud").join("scratch"),
                )),
            };
        *guard = Some(owo_agent_core::cloud_exec::CloudTaskQueue::new(
            dir, transport,
        ));
    }
    Ok(guard)
}

/// 提交云端任务：`POST /cloud/tasks`（body = CloudTaskSpec；入队后立即执行一轮）。
async fn cloud_task_submit(
    State(state): State<Arc<AppState>>,
    Json(spec): Json<owo_agent_core::cloud_exec::CloudTaskSpec>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut guard = cloud_queue(&state)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let queue = guard.as_mut().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "云端队列未初始化".to_string(),
    ))?;
    let task_id = queue
        .submit(spec)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    // 第四轮接线：进度经 SSE 集线器发布，前端以同一 task_id 订阅
    // /cloud/tasks/{id}/events（历史重放 + 实时帧）。
    let sink = sse::sink(task_id.clone());
    queue
        .run_next(&sink)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let record = queue
        .record(&task_id)
        .cloned()
        .ok_or((StatusCode::NOT_FOUND, format!("任务 {task_id} 不存在")))?;
    Ok(Json(
        json!({ "ok": true, "task": record, "transport": queue.transport_kind() }),
    ))
}

/// 查询云端任务：`GET /cloud/tasks/{id}`。
async fn cloud_task_status(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let guard = cloud_queue(&state)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let queue = guard.as_ref().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "云端队列未初始化".to_string(),
    ))?;
    let record = queue
        .record(&id)
        .ok_or((StatusCode::NOT_FOUND, format!("云端任务 {id} 不存在")))?;
    let usage = queue.usage(&id);
    Ok(Json(json!({
        "state": format!("{:?}", record.state),
        "retry_count": record.retry_count,
        "last_error": record.last_error,
        "created_at": record.created_at,
        "duration_ms": record.duration_ms,
        "usage": usage,
    })))
}

/// 获取云端任务结果：`GET /cloud/tasks/{id}/result`。
async fn cloud_task_result(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let guard = cloud_queue(&state)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let queue = guard.as_ref().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "云端队列未初始化".to_string(),
    ))?;
    let record = queue
        .record(&id)
        .ok_or((StatusCode::NOT_FOUND, format!("云端任务 {id} 不存在")))?;
    let result = record
        .result
        .clone()
        .ok_or((StatusCode::CONFLICT, format!("云端任务 {id} 尚无结果")))?;
    Ok(Json(
        json!({ "ok": true, "result": result, "diff_summary": owo_agent_core::cloud_exec::describe_diff(&result.diff) }),
    ))
}

/// 取消云端任务：`POST /cloud/tasks/{id}/cancel`。
async fn cloud_task_cancel(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut guard = cloud_queue(&state)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let queue = guard.as_mut().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "云端队列未初始化".to_string(),
    ))?;
    queue
        .cancel(&id)
        .await
        .map(|_| Json(json!({ "ok": true, "task_id": id })))
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

fn to_event(sse: SseEvent) -> Result<Event, Infallible> {
    let name = match &sse {
        SseEvent::Progress { .. } => "progress",
        SseEvent::ToolUse { .. } => "tool_use",
        SseEvent::ToolResult { .. } => "tool_result",
        SseEvent::PermissionRequest { .. } => "permission_request",
        SseEvent::Final { .. } => "final",
        SseEvent::TokenDelta { .. } => "token_delta",
        SseEvent::Compaction { .. } => "compaction",
    };
    // R10：SSE 事件统一携带协议版本 v（见 protocol::SSE_PROTOCOL_VERSION）。
    let mut payload = serde_json::to_value(&sse).unwrap_or_else(|_| json!({}));
    if let serde_json::Value::Object(map) = &mut payload {
        map.insert(
            "v".to_string(),
            json!(owo_agent_protocol::SSE_PROTOCOL_VERSION),
        );
    }
    let data = payload.to_string();
    Ok(Event::default().event(name).data(data))
}

// ===========================================================================
// R10 契约治理：API 版本 / 弃用策略 / 错误码表 / JSON Schema 发布
// ===========================================================================

/// API 版本（`x-owo-api-version`）。破坏性变更递增 minor；弃用期 ≥2 个 minor。
pub const OWO_API_VERSION: &str = "0.7";

/// 已弃用路由登记：(路径前缀, since, until, 替代建议)。
/// 命中时响应携带 `Deprecation` 头；当前无已弃用路由，破坏性变更前在此登记。
const DEPRECATED_ROUTES: &[(&str, &str, &str, &str)] = &[];

/// 路由/事件契约变更 RFC 登记（弃用策略落地：变更前登记 → 弃用期 ≥2 minor → 移除）：
/// - 2026-08-17（R10）：SSE 事件 data 统一携带 `v` 字段（v=1；旧客户端帧缺 v 视为 v=0）。
/// - 2026-08-17（R10）：新增 /schemas/{kind}/{version} 静态 JSON Schema 版本化发布。
/// - 2026-08-17（R10）：错误响应统一为 {error:{code,message,retry_after_ms,domain,reason,retryable}}。
#[allow(dead_code)]
const CONTRACT_RFC_LOG: &str = "2026-08-17 R10: SSE v 字段 / schemas 发布 / 统一错误码";

/// 统一错误响应（R10 错误码表接入 HTTP 层）：(status, {error:{code,message,retry_after_ms,...}})。
fn api_error_response(
    code: &error_codes::ErrorCode,
    message: impl std::fmt::Display,
) -> (StatusCode, Json<Value>) {
    (
        StatusCode::from_u16(code.http_status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(json!({
            "error": {
                "code": format!(
                    "{}/{}/{}",
                    code.domain,
                    code.reason,
                    if code.retryable { "retryable" } else { "not_retryable" }
                ),
                "message": message.to_string(),
                "domain": code.domain,
                "reason": code.reason,
                "retryable": code.retryable,
                "retry_after_ms": code.retry_after_ms,
            }
        })),
    )
}

/// 计算给定路径应附加的 `Deprecation` 头值（未命中返回 None）。
/// 独立为纯函数供契约测试直接覆盖命中/未命中与头格式（R12 收尾）。
pub fn deprecation_header_value_for(
    routes: &[(&str, &str, &str, &str)],
    path: &str,
) -> Option<String> {
    for (route, since, until, alternative) in routes {
        if path.starts_with(route) {
            return Some(format!(
                "{route}: since {since}, until {until} (use {alternative})"
            ));
        }
    }
    None
}

/// Deprecation 中间件：命中 DEPRECATED_ROUTES 的请求附加 `Deprecation` 响应头。
async fn deprecation_middleware(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let path = request.uri().path().to_string();
    let mut response = next.run(request).await;
    if let Some(value) = deprecation_header_value_for(DEPRECATED_ROUTES, &path) {
        if let Ok(value) = value.parse::<axum::http::HeaderValue>() {
            response.headers_mut().insert("Deprecation", value);
        }
    }
    response
}

/// /schemas 列表（R10：JSON Schema 版本化发布索引）。
async fn schemas_list() -> Json<Value> {
    Json(json!({
        "api_version": OWO_API_VERSION,
        "schemas": {
            "plugin-manifest": ["v1"],
            "owskill": ["v1"],
            "owflow": ["v1"],
        },
        "note": "GET /schemas/{kind}/{version} 获取 JSON Schema（draft-07）",
    }))
}

const SCHEMA_PLUGIN_MANIFEST_V1: &str = r#"{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "https://owo.local/schemas/plugin-manifest/v1",
  "title": "OwO Plugin Manifest",
  "type": "object",
  "required": ["id", "name", "version"],
  "properties": {
    "id": { "type": "string", "minLength": 1 },
    "name": { "type": "string", "minLength": 1 },
    "version": { "type": "string", "pattern": "^[0-9]+\\.[0-9]+\\.[0-9]+" },
    "description": { "type": "string" },
    "permissions": { "type": "array", "items": { "type": "string" } },
    "mcp": { "type": "object" },
    "min_app_version": { "type": "string" },
    "entry": { "type": "string" },
    "network_allowlist": { "type": "array", "items": { "type": "string" } },
    "signature": { "type": "string" }
  },
  "additionalProperties": false
}"#;

const SCHEMA_OWSKILL_V1: &str = r#"{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "https://owo.local/schemas/owskill/v1",
  "title": "OwO Flow Skill Package (.owskill)",
  "type": "object",
  "required": ["manifest", "graph", "skill_md"],
  "properties": {
    "manifest": {
      "type": "object",
      "required": ["id", "name", "version", "min_app_version", "target_apps", "sensitivity"],
      "properties": {
        "id": { "type": "string", "minLength": 1 },
        "name": { "type": "string", "minLength": 1 },
        "version": { "type": "string", "pattern": "^[0-9]+\\.[0-9]+\\.[0-9]+" },
        "min_app_version": { "type": "string" },
        "target_apps": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
        "permissions": { "type": "array", "items": { "type": "string" } },
        "variables": { "type": "array", "items": { "type": "string" } },
        "sensitivity": { "enum": ["none", "low", "medium", "high"] }
      },
      "additionalProperties": false
    },
    "graph": { "type": "object" },
    "skill_md": { "type": "string" }
  },
  "additionalProperties": false
}"#;

const SCHEMA_OWFLOW_V1: &str = r#"{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "https://owo.local/schemas/owflow/v1",
  "title": "OwO Workflow Definition (.owflow)",
  "type": "object",
  "required": ["id", "name"],
  "properties": {
    "id": { "type": "string", "minLength": 1 },
    "name": { "type": "string", "minLength": 1 },
    "version": { "type": "integer", "minimum": 1 },
    "triggers": {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["id", "kind"],
        "properties": {
          "id": { "type": "string" },
          "kind": { "type": "object" }
        }
      }
    },
    "permissions": {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["scope", "mode"],
        "properties": {
          "scope": { "type": "string" },
          "mode": { "enum": ["allow", "ask", "deny"] }
        }
      }
    },
    "preconditions": { "type": "array", "items": { "type": "string" } },
    "rollback_points": { "type": "array", "items": { "type": "string" } },
    "max_steps": { "type": "integer", "minimum": 1 },
    "subflow_depth_limit": { "type": "integer", "minimum": 1 },
    "steps": { "type": "array", "items": { "type": "object" } }
  },
  "additionalProperties": false
}"#;

/// GET /schemas/{kind}/{version}：静态 JSON Schema（版本化发布）。
async fn schema_get(
    AxumPath((kind, version)): AxumPath<(String, String)>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let raw = match (kind.as_str(), version.as_str()) {
        ("plugin-manifest", "v1") => SCHEMA_PLUGIN_MANIFEST_V1,
        ("owskill", "v1") => SCHEMA_OWSKILL_V1,
        ("owflow", "v1") => SCHEMA_OWFLOW_V1,
        _ => {
            return Err((
                StatusCode::NOT_FOUND,
                format!("未知 schema：{kind}/{version}（GET /schemas 查看列表）"),
            ))
        }
    };
    let value: Value = serde_json::from_str(raw)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(value))
}
