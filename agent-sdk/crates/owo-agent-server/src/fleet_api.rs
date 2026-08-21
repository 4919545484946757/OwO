// R12:fleet_api 完成，待主控接线
//! 控制面 HTTP 契约（P2 双节点网格第一阶段）：节点注册/心跳、任务提交/查询/取消/SSE、审批响应。
//!
//! 路由（前缀 /fleet，待主控在 `lib.rs::build_router` 挂载 `fleet_api::router(state)`）：
//! - `POST /fleet/nodes/register`        节点注册（CapabilityCard + 心跳续租）
//! - `GET  /fleet/nodes`                 节点列表（NodeStatus 快照）
//! - `POST /fleet/tasks/submit`          任务提交（`Idempotency-Key` 头幂等）
//! - `GET  /fleet/tasks/{id}`            任务状态 + 事件
//! - `POST /fleet/tasks/{id}/cancel`     取消任务
//! - `GET  /fleet/tasks/{id}/events`     SSE（历史重放 + 实时；`?format=json` 拉全量）
//! - `POST /fleet/approvals/{id}/respond` 审批响应（影响预览 + 结构化证据齐备才批准）
//!
//! 运行态：模块内 `OnceLock` 单例 [`FleetHub`]（进程内 [`InMemoryTransport`] 承载任务执行、
//! [`LeaseManager`] 节点租约/fencing、[`AgentBus`]+[`BusStore`] 节点/任务事件持久化、
//! [`CasStore`] 产物、[`ExperienceStore`] 节点状态变迁）。后台节点执行器把 Running 任务
//! 交由匹配节点完成（两节点模拟：注册 node-a/node-b 后任务自动执行）。
//!
//! 协议约束：本模块不引用 `crate::`/`super::`；`AppState` 全限定名 `owo_agent_server::AppState`；
//! 错误统一 `(StatusCode, Json({error}))`；不给 AppState 加字段（状态在模块内）。

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use owo_agent_core::bus_store::BusStore;
use owo_agent_core::capability::{CapabilityCard, CapabilityWorkerRegistry};
use owo_agent_core::cas_store::CasStore;
use owo_agent_core::experience_store::ExperienceStore;
use owo_agent_core::fleet::AgentBus;
use owo_agent_core::fleet_transport::{
    FleetTransport, InMemoryTransport, TransportEvent, TransportStatus, TransportTask,
};
use owo_agent_core::lease::{LeaseConfig, LeaseManager};
use owo_agent_core::node_agent::{NodeAgent, NodeStatus};
use owo_agent_core::remote_step::EvidenceItem;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio_stream::StreamExt;

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<serde_json::Value>)>;

fn api_err(
    status: StatusCode,
    message: impl Into<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    (status, Json(serde_json::json!({ "error": message.into() })))
}

// ---------- SSE 集线器（任务事件：历史重放 + 实时） ----------

/// 任务事件 SSE：task_id → 广播通道 + 历史（订阅先重放历史再流式）。
#[derive(Default)]
pub struct FleetSse {
    senders: Mutex<HashMap<String, broadcast::Sender<String>>>,
    history: Mutex<HashMap<String, Vec<String>>>,
}

impl FleetSse {
    pub fn new() -> Self {
        Self::default()
    }

    fn publish(&self, task_id: &str, frame: String) {
        if let Ok(mut history) = self.history.lock() {
            history
                .entry(task_id.to_string())
                .or_default()
                .push(frame.clone());
        }
        if let Ok(senders) = self.senders.lock() {
            if let Some(sender) = senders.get(task_id) {
                let _ = sender.send(frame);
            }
        }
    }

    /// 订阅：返回（广播接收端，历史帧）。
    pub fn subscribe(&self, task_id: &str) -> (broadcast::Receiver<String>, Vec<String>) {
        let sender = {
            let mut senders = self.senders.lock().unwrap_or_else(|e| e.into_inner());
            senders
                .entry(task_id.to_string())
                .or_insert_with(|| broadcast::channel(256).0)
                .clone()
        };
        let history = self
            .history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(task_id)
            .cloned()
            .unwrap_or_default();
        (sender.subscribe(), history)
    }
}

// ---------- FleetHub：控制面运行态 ----------

/// 审批记录（影响预览 + 结构化证据齐备才批准）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub approval_id: String,
    pub task_id: String,
    pub step_id: String,
    pub owner_device: String,
    pub summary: String,
    pub impact_preview: String,
    pub evidence: Vec<EvidenceItem>,
    pub decided: bool,
    pub decision: Option<String>,
    pub approved_by: Option<String>,
}

/// 控制面运行态。
pub struct FleetHub {
    pub nodes: Mutex<HashMap<String, Arc<NodeAgent>>>,
    pub approvals: Mutex<HashMap<String, ApprovalRecord>>,
    pub transport: InMemoryTransport,
    pub leases: LeaseManager,
    pub bus: AgentBus,
    pub bus_store: BusStore,
    pub experience: ExperienceStore,
    /// R12 节点显式驱动阶段尚未消费 CAS（产物写入在 R13 经 HttpTransport 节点进程接线）；
    /// 保留字段以维持控制面"内容寻址产物"契约。
    #[allow(dead_code)]
    pub cas: CasStore,
    pub registry: CapabilityWorkerRegistry,
    pub sse: FleetSse,
}

impl FleetHub {
    /// 新建控制面运行态（持久化目录 data_root/fleet；测试可独立构造，避免跨测试污染）。
    pub fn new(data_root: &std::path::Path) -> Result<Arc<FleetHub>, String> {
        let fleet_dir = data_root.join("fleet");
        let bus = AgentBus::new();
        let bus_store = BusStore::new(Some(fleet_dir.join("bus.jsonl")))?;
        // 运行时挂接总线持久化（关键消息自动落盘；独立任务避免同步初始化阻塞）。
        {
            let bus2 = bus.clone();
            let store2 = bus_store.clone();
            tokio::spawn(async move {
                bus2.attach_store(store2).await;
            });
        }
        let cas = CasStore::new(fleet_dir.join("cas"))?;
        let experience = ExperienceStore::new(Some(fleet_dir.join("experience.jsonl")))?;
        Ok(Arc::new(FleetHub {
            nodes: Mutex::new(HashMap::new()),
            approvals: Mutex::new(HashMap::new()),
            transport: InMemoryTransport::with_ttl(Duration::from_secs(120)),
            leases: LeaseManager::with_config(LeaseConfig {
                ttl_secs: 60,
                renew_interval_secs: 20,
            }),
            bus,
            bus_store,
            experience,
            cas,
            registry: CapabilityWorkerRegistry::new(),
            sse: FleetSse::new(),
        }))
    }
}

/// 进程级控制面运行态（生产：Agent 1 挂载 `fleet_api::router` 时初始化；幂等）。
/// 测试用 [`FleetHub::new`] 独立构造，故本函数在未挂载前标记 dead_code。
#[allow(dead_code)]
pub fn fleet_hub(data_root: &std::path::Path) -> Arc<FleetHub> {
    static HUB: OnceLock<Arc<FleetHub>> = OnceLock::new();
    HUB.get_or_init(|| {
        FleetHub::new(data_root).unwrap_or_else(|e| panic!("fleet hub 初始化失败：{e}"))
    })
    .clone()
}

/// 节点注册请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterNodeBody {
    pub node_id: String,
    pub card: CapabilityCard,
}

/// 任务提交请求（直接内联 TransportTask 字段，便于契约稳定）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitTaskBody {
    pub task_id: String,
    pub worker: String,
    pub input: serde_json::Value,
    #[serde(default)]
    pub correlation_id: String,
    #[serde(default)]
    pub lineage: Vec<String>,
    #[serde(default)]
    pub approval_required: bool,
}

/// 审批响应请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRespondBody {
    pub decision: String,
    pub approved_by: String,
}

/// 任务查询结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskView {
    pub task_id: String,
    pub worker: String,
    pub correlation_id: String,
    pub status: TransportStatus,
    pub events: Vec<TransportEvent>,
    #[serde(default)]
    pub approval: Option<ApprovalRecord>,
}

// ---------- 路由 ----------

/// 组装 fleet 路由（handler 状态 = 独立 [`FleetHub`]；生产经 [`router`] 用进程级 hub）。
pub fn router_with_hub(hub: Arc<FleetHub>) -> Router {
    Router::new()
        .route("/fleet/nodes/register", post(register_node))
        .route("/fleet/nodes", get(list_nodes))
        .route("/fleet/tasks/submit", post(submit_task))
        .route("/fleet/tasks/{id}", get(get_task))
        .route("/fleet/tasks/{id}/cancel", post(cancel_task))
        .route("/fleet/tasks/{id}/events", get(task_events))
        .route("/fleet/approvals/{id}/respond", post(respond_approval))
        .with_state(hub)
}

/// 组装 fleet 路由（待主控在 build_router merge；data_root 用于控制面持久化目录）。
/// 测试用 [`router_with_hub`]，故本函数在未挂载前标记 dead_code。
#[allow(dead_code)]
pub fn router(state: Arc<owo_agent_server::AppState>) -> Router {
    router_with_hub(fleet_hub(&state.data_root))
}

/// 后台节点执行器说明：R12 第一阶段任务执行由"节点"显式驱动（模拟冒烟中测试扮演节点，
/// 经 `hub.transport.complete_task` 完成；真实节点进程在 R13 经 HttpTransport 接线）。
impl FleetHub {
    /// 生成任务视图（从传输层读状态/事件）。
    fn task_view(&self, task_id: &str) -> Option<TaskView> {
        let task = self.transport.task(task_id);
        let status = self.transport.task_status(task_id)?;
        let events = self.transport.task_events(task_id);
        let approval = self
            .approvals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(task_id)
            .cloned();
        let (worker, correlation_id) = match &task {
            Some(t) => (t.worker.clone(), t.correlation_id.clone()),
            None => (String::new(), String::new()),
        };
        Some(TaskView {
            task_id: task_id.to_string(),
            worker,
            correlation_id,
            status,
            events,
            approval,
        })
    }
}

// ---------- 节点 ----------

async fn register_node(
    State(hub): State<Arc<FleetHub>>,
    Json(body): Json<RegisterNodeBody>,
) -> ApiResult<serde_json::Value> {
    let node_id = body.node_id.trim().to_string();
    if node_id.is_empty() {
        return Err(api_err(StatusCode::BAD_REQUEST, "node_id 不能为空"));
    }
    let existing = {
        let nodes = hub.nodes.lock().unwrap_or_else(|e| e.into_inner());
        nodes.get(&node_id).cloned()
    };
    let node = match existing {
        Some(node) => {
            // 幂等重注册 = 心跳续租（复用现有租约 token）。
            node.heartbeat_and_report_persisted(&hub.registry).await;
            node
        }
        None => {
            let node = Arc::new(NodeAgent::with_timeout(
                node_id.clone(),
                body.card.clone(),
                Duration::from_secs(3),
                owo_agent_core::fleet::RestartRule::default(),
            ));
            node.attach_control_plane(hub.leases.clone(), hub.bus.clone(), hub.experience.clone());
            let lease = node
                .register_with_control_plane(&hub.registry)
                .await
                .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
            let _ = owo_agent_core::bus_store::persist_node_status(
                &hub.bus_store,
                &node_id,
                true,
                "节点注册",
            );
            hub.sse.publish(
                &node_id,
                format!(
                    "data: {}\n\n",
                    serde_json::json!({ "event": "node_registered", "node_id": node_id, "lease_epoch": lease.epoch })
                ),
            );
            if let Ok(mut nodes) = hub.nodes.lock() {
                nodes.insert(node_id.clone(), node.clone());
            }
            node
        }
    };
    Ok(Json(serde_json::json!({
        "node_id": node_id,
        "status": node.status(),
        "lease_epoch": node.status().lease_epoch,
    })))
}

async fn list_nodes(State(hub): State<Arc<FleetHub>>) -> ApiResult<serde_json::Value> {
    let nodes = hub.nodes.lock().unwrap_or_else(|e| e.into_inner());
    let list: Vec<NodeStatus> = nodes.values().map(|n| n.status()).collect();
    Ok(Json(
        serde_json::json!({ "nodes": list, "count": list.len() }),
    ))
}

// ---------- 任务 ----------

async fn submit_task(
    State(hub): State<Arc<FleetHub>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<SubmitTaskBody>,
) -> ApiResult<serde_json::Value> {
    let task_id = body.task_id.trim().to_string();
    if task_id.is_empty() {
        return Err(api_err(StatusCode::BAD_REQUEST, "task_id 不能为空"));
    }
    // 幂等键：Idempotency-Key 头优先，否则 task_id 本身（transport 拒绝重复提交）。
    let idempotency = headers
        .get("Idempotency-Key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or(&task_id)
        .to_string();
    let task = TransportTask {
        task_id: task_id.clone(),
        worker: body.worker.clone(),
        input: body.input.clone(),
        correlation_id: if body.correlation_id.is_empty() {
            format!("fleet:{}", task_id)
        } else {
            body.correlation_id.clone()
        },
        lineage: body.lineage.clone(),
        approval_required: body.approval_required,
    };
    // 审批任务：登记审批记录（影响预览 + 结构化证据由调用方以 input.approval 提供）。
    if body.approval_required {
        let approval = ApprovalRecord {
            approval_id: idempotency.clone(),
            task_id: task_id.clone(),
            step_id: body
                .input
                .pointer("/step_id")
                .and_then(|v| v.as_str())
                .unwrap_or(&task_id)
                .to_string(),
            owner_device: body
                .input
                .pointer("/approval/owner_device")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            summary: body
                .input
                .pointer("/approval/summary")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            impact_preview: body
                .input
                .pointer("/impact_preview")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            evidence: body
                .input
                .pointer("/evidence")
                .and_then(|v| serde_json::from_value::<Vec<EvidenceItem>>(v.clone()).ok())
                .unwrap_or_default(),
            decided: false,
            decision: None,
            approved_by: None,
        };
        if let Ok(mut approvals) = hub.approvals.lock() {
            approvals.insert(task_id.clone(), approval);
        }
    }
    hub.transport
        .submit(task)
        .await
        .map_err(|e| api_err(StatusCode::CONFLICT, e))?;
    // 总线持久化（关键消息落盘；崩溃重放恢复）。
    let _ = owo_agent_core::bus_store::persist_remote_event(
        &hub.bus_store,
        &owo_agent_core::remote_step::RemoteStepEvent::Submitted {
            step_id: task_id.clone(),
            correlation_id: body.correlation_id.clone(),
            worker: body.worker.clone(),
        },
    );
    let status = hub
        .transport
        .task_status(&task_id)
        .unwrap_or(TransportStatus::Pending);
    Ok(Json(serde_json::json!({
        "task_id": task_id,
        "status": status,
        "idempotency_key": idempotency,
    })))
}

async fn get_task(
    State(hub): State<Arc<FleetHub>>,
    Path(task_id): Path<String>,
) -> ApiResult<TaskView> {
    hub.task_view(&task_id)
        .map(Json)
        .ok_or_else(|| api_err(StatusCode::NOT_FOUND, format!("未知任务：{task_id}")))
}

async fn cancel_task(
    State(hub): State<Arc<FleetHub>>,
    Path(task_id): Path<String>,
) -> ApiResult<serde_json::Value> {
    hub.transport
        .cancel(&task_id)
        .await
        .map_err(|e| api_err(StatusCode::NOT_FOUND, e))?;
    hub.sse.publish(
        &task_id,
        format!(
            "data: {}\n\n",
            serde_json::json!({ "event": "cancelled", "task_id": task_id })
        ),
    );
    Ok(Json(
        serde_json::json!({ "task_id": task_id, "status": "cancelled" }),
    ))
}

/// SSE 事件查询参数。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EventsQuery {
    /// `json` 时返回一次性 JSON 数组（供 HttpTransport::events 拉取）。
    #[serde(default)]
    pub format: Option<String>,
}

async fn task_events(
    State(hub): State<Arc<FleetHub>>,
    Path(task_id): Path<String>,
    Query(query): Query<EventsQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    if hub.transport.task_status(&task_id).is_none() {
        return Err(api_err(
            StatusCode::NOT_FOUND,
            format!("未知任务：{task_id}"),
        ));
    }
    if query.format.as_deref() == Some("json") {
        let events = hub.transport.task_events(&task_id);
        return Ok(Json(events).into_response());
    }
    // SSE：历史重放 + 实时。
    let (rx, history) = hub.sse.subscribe(&task_id);
    let stream = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|item| match item {
        Ok(frame) => Some(Ok::<Event, Infallible>(Event::default().data(frame))),
        Err(_) => None,
    });
    // 历史帧先行。
    let history_stream = tokio_stream::iter(history)
        .map(|frame| Ok::<Event, Infallible>(Event::default().data(frame)));
    let combined = history_stream.chain(stream);
    Ok(Sse::new(combined).into_response())
}

// ---------- 审批 ----------

async fn respond_approval(
    State(hub): State<Arc<FleetHub>>,
    Path(task_id): Path<String>,
    Json(body): Json<ApprovalRespondBody>,
) -> ApiResult<serde_json::Value> {
    let approval = {
        let approvals = hub.approvals.lock().unwrap_or_else(|e| e.into_inner());
        approvals.get(&task_id).cloned()
    };
    let Some(mut approval) = approval else {
        return Err(api_err(
            StatusCode::NOT_FOUND,
            format!("任务 {task_id} 不是审批任务或无审批记录"),
        ));
    };
    if approval.decided {
        return Err(api_err(StatusCode::CONFLICT, "审批已裁决"));
    }
    let status = hub
        .transport
        .task_status(&task_id)
        .unwrap_or(TransportStatus::Pending);
    if !matches!(status, TransportStatus::AwaitingApproval) {
        return Err(api_err(
            StatusCode::CONFLICT,
            format!("任务 {task_id} 不在审批等待态（当前 {status:?}）"),
        ));
    }
    match body.decision.as_str() {
        "approve" => {
            // 影响预览 + 结构化证据齐备才批准（否则拒绝执行）。
            if approval.impact_preview.trim().is_empty() || approval.evidence.is_empty() {
                approval.decided = true;
                approval.decision = Some("rejected".to_string());
                approval.approved_by = Some(body.approved_by.clone());
                let _ = hub
                    .transport
                    .deny_task(&task_id, "审批材料不齐（缺影响预览或证据）");
                if let Ok(mut approvals) = hub.approvals.lock() {
                    approvals.insert(task_id.clone(), approval.clone());
                }
                hub.sse.publish(
                    &task_id,
                    format!(
                        "data: {}\n\n",
                        serde_json::json!({ "event": "approval_rejected", "task_id": task_id })
                    ),
                );
                return Err(api_err(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "审批材料不齐：需影响预览 + 结构化证据",
                ));
            }
            if !hub.transport.approve_task(&task_id, &body.approved_by) {
                return Err(api_err(StatusCode::INTERNAL_SERVER_ERROR, "审批放行失败"));
            }
            approval.decided = true;
            approval.decision = Some("approved".to_string());
            approval.approved_by = Some(body.approved_by.clone());
            if let Ok(mut approvals) = hub.approvals.lock() {
                approvals.insert(task_id.clone(), approval.clone());
            }
            hub.sse.publish(
                &task_id,
                format!(
                    "data: {}\n\n",
                    serde_json::json!({ "event": "approval_granted", "task_id": task_id })
                ),
            );
            Ok(Json(serde_json::json!({
                "task_id": task_id,
                "decision": "approved",
                "status": hub.transport.task_status(&task_id),
            })))
        }
        "reject" => {
            if !hub
                .transport
                .deny_task(&task_id, &format!("用户拒绝：{}", body.approved_by))
            {
                return Err(api_err(StatusCode::INTERNAL_SERVER_ERROR, "审批拒绝失败"));
            }
            approval.decided = true;
            approval.decision = Some("rejected".to_string());
            approval.approved_by = Some(body.approved_by.clone());
            if let Ok(mut approvals) = hub.approvals.lock() {
                approvals.insert(task_id.clone(), approval.clone());
            }
            hub.sse.publish(
                &task_id,
                format!(
                    "data: {}\n\n",
                    serde_json::json!({ "event": "approval_rejected", "task_id": task_id })
                ),
            );
            Ok(Json(serde_json::json!({
                "task_id": task_id,
                "decision": "rejected",
                "status": "cancelled",
            })))
        }
        other => Err(api_err(
            StatusCode::BAD_REQUEST,
            format!("decision 必须是 approve/reject，实际 {other}"),
        )),
    }
}
