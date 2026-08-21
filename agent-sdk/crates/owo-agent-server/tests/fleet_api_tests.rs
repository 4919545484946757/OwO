//! 控制面 HTTP 契约测试（P2 双节点网格第一阶段，最小契约）。
//!
//! `#[path = "../src/fleet_api.rs"] mod fleet_api;` 独立编译；
//! 每个测试独立构造 [`FleetHub`]（tempfile 临时目录），避免跨测试状态污染；
//! 节点执行由测试显式驱动（`hub.transport.complete_task`，模拟节点 agent 产出）。
//!
//! 覆盖：节点注册/列表、任务提交/完成/取消、审批（影响预览 + 结构化证据齐备才批准）、
//! 两节点冒烟链（注册→提交→租约→fencing→远程 step→审批→重放，无孤儿、无重复执行）。

#[path = "../src/fleet_api.rs"]
mod fleet_api;

use axum::body::Body;
use axum::http::{header, Method, Request, Response, StatusCode};
use owo_agent_core::fleet_transport::TransportStatus;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

type Hub = Arc<fleet_api::FleetHub>;

async fn test_hub() -> (Hub, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let hub = fleet_api::FleetHub::new(temp.path()).unwrap();
    (hub, temp)
}

fn request(method: &str, path: &str, body: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(path);
    if let Some(b) = body {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
        return builder.body(Body::from(b.to_string())).unwrap();
    }
    builder.body(Body::empty()).unwrap()
}

async fn send(hub: Hub, method: &str, path: &str, body: Option<&str>) -> Response<Body> {
    fleet_api::router_with_hub(hub)
        .oneshot(request(method, path, body))
        .await
        .unwrap()
}

async fn body_json(response: Response<Body>) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn register_body(node_id: &str) -> String {
    json!({
        "node_id": node_id,
        "card": {
            "worker": node_id,
            "os": "windows",
            "arch": "x86_64",
            "actions": ["shell".to_string()],
        }
    })
    .to_string()
}

fn submit_body(task_id: &str, worker: &str, approval: Option<Value>) -> String {
    let approval_required = approval.is_some();
    let mut input = json!({ "q": 1 });
    if let Some(a) = approval {
        input = a;
    }
    json!({
        "task_id": task_id,
        "worker": worker,
        "input": input,
        "correlation_id": format!("corr:{task_id}"),
        "approval_required": approval_required,
    })
    .to_string()
}

/// 审批材料齐备的 input。
fn approval_input(task_id: &str, owner: &str) -> Value {
    json!({
        "step_id": task_id,
        "approval": {
            "required": true,
            "owner_device": owner,
            "summary": "远程执行点击",
        },
        "impact_preview": "修改 config.yaml、重启目标服务",
        "evidence": [
            { "kind": "file_diff", "summary": "config.yaml +2 行" },
            { "kind": "command", "summary": "重启 service-a" }
        ]
    })
}

/// 轮询任务直到终态或超时。
async fn wait_status(
    hub: Hub,
    task_id: &str,
    expect: TransportStatus,
    max_wait: Duration,
) -> Value {
    let deadline = std::time::Instant::now() + max_wait;
    loop {
        let resp = send(hub.clone(), "GET", &format!("/fleet/tasks/{task_id}"), None).await;
        assert_eq!(resp.status(), StatusCode::OK, "任务查询应 200");
        let view = body_json(resp).await;
        let status = view["status"].as_str().unwrap_or("");
        let parsed = serde_json::from_value::<TransportStatus>(json!(status)).unwrap();
        if parsed == expect {
            return view;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "等待任务 {task_id} 到达 {expect:?} 超时（当前 {status}）"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

/// 1) 节点注册 + 列表（CapabilityCard 自报 + 租约）。
#[tokio::test]
async fn register_and_list_nodes() {
    let (hub, _temp) = test_hub().await;
    for node in ["node-a", "node-b"] {
        let resp = send(
            hub.clone(),
            "POST",
            "/fleet/nodes/register",
            Some(&register_body(node)),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"]["registered"], true, "节点应已注册");
        assert!(
            body["lease_epoch"].as_u64().unwrap_or(0) > 0,
            "注册应持租约"
        );
    }
    let resp = send(hub.clone(), "GET", "/fleet/nodes", None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["count"], 2);
    let ids: Vec<&str> = body["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"node-a") && ids.contains(&"node-b"));
}

/// 2) 提交 → 节点显式完成 → Succeeded（无孤儿挂起、无重复完成）。
#[tokio::test]
async fn submit_and_complete() {
    let (hub, _temp) = test_hub().await;
    let task_id = new_id("t");
    let resp = send(
        hub.clone(),
        "POST",
        "/fleet/tasks/submit",
        Some(&submit_body(&task_id, "node-a", None)),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    // 节点执行：显式完成（模拟 node-a 产出）。
    assert!(
        hub.transport
            .complete_task(&task_id, true, json!("out-from-node")),
        "任务应可完成"
    );
    let view = wait_status(
        hub.clone(),
        &task_id,
        TransportStatus::Succeeded,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(view["worker"], "node-a");
    let kinds: Vec<&str> = view["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"result"), "完成事件应落 Result：{kinds:?}");
    // 幂等完成：终态任务不再完成（无重复事件）。
    assert!(
        !hub.transport.complete_task(&task_id, true, json!("again")),
        "终态任务不应重复完成"
    );
}

/// 3) 审批：影响预览 + 结构化证据齐备 → 批准 → 节点执行完成。
#[tokio::test]
async fn approval_with_material_executes() {
    let (hub, _temp) = test_hub().await;
    let task_id = new_id("rs");
    let input = approval_input(&task_id, "phone-1");
    let resp = send(
        hub.clone(),
        "POST",
        "/fleet/tasks/submit",
        Some(&submit_body(&task_id, "node-a", Some(input))),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    // 审批未决。
    let view = wait_status(
        hub.clone(),
        &task_id,
        TransportStatus::AwaitingApproval,
        Duration::from_secs(3),
    )
    .await;
    assert!(view["approval"]["impact_preview"]
        .as_str()
        .unwrap()
        .contains("config.yaml"));
    assert_eq!(view["approval"]["evidence"].as_array().unwrap().len(), 2);
    // 批准放行。
    let resp = send(
        hub.clone(),
        "POST",
        &format!("/fleet/approvals/{task_id}/respond"),
        Some(&json!({ "decision": "approve", "approved_by": "user-1" }).to_string()),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    // 审批放行后节点执行：显式完成。
    assert!(
        hub.transport
            .complete_task(&task_id, true, json!("out-from-node")),
        "审批放行后任务应可完成"
    );
    let view = wait_status(
        hub.clone(),
        &task_id,
        TransportStatus::Succeeded,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(view["approval"]["decision"], "approved");
}

/// 4) 审批材料不齐（缺证据）→ 批准被拒（显式 422，任务取消，不静默执行）。
#[tokio::test]
async fn approval_missing_material_rejected() {
    let (hub, _temp) = test_hub().await;
    let task_id = new_id("rs");
    // 缺 evidence。
    let input = json!({
        "step_id": task_id,
        "approval": { "required": true, "owner_device": "phone-1", "summary": "x" },
        "impact_preview": "只有预览无证据"
    });
    let resp = send(
        hub.clone(),
        "POST",
        "/fleet/tasks/submit",
        Some(&submit_body(&task_id, "node-a", Some(input))),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = send(
        hub.clone(),
        "POST",
        &format!("/fleet/approvals/{task_id}/respond"),
        Some(&json!({ "decision": "approve", "approved_by": "user-1" }).to_string()),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "材料不齐应拒绝批准"
    );
    let view = wait_status(
        hub.clone(),
        &task_id,
        TransportStatus::Cancelled,
        Duration::from_secs(3),
    )
    .await;
    assert_eq!(view["approval"]["decision"], "rejected");
}

/// 5) 审批拒绝决策 → 任务取消。
#[tokio::test]
async fn approval_reject_decision_cancels() {
    let (hub, _temp) = test_hub().await;
    let task_id = new_id("rs");
    let input = approval_input(&task_id, "phone-1");
    let resp = send(
        hub.clone(),
        "POST",
        "/fleet/tasks/submit",
        Some(&submit_body(&task_id, "node-a", Some(input))),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = send(
        hub.clone(),
        "POST",
        &format!("/fleet/approvals/{task_id}/respond"),
        Some(&json!({ "decision": "reject", "approved_by": "user-1" }).to_string()),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let view = wait_status(
        hub.clone(),
        &task_id,
        TransportStatus::Cancelled,
        Duration::from_secs(3),
    )
    .await;
    assert_eq!(view["approval"]["decision"], "rejected");
}

/// 6) 取消任务。
#[tokio::test]
async fn cancel_task() {
    let (hub, _temp) = test_hub().await;
    let task_id = new_id("t");
    let resp = send(
        hub.clone(),
        "POST",
        "/fleet/tasks/submit",
        Some(&submit_body(&task_id, "node-a", None)),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = send(
        hub.clone(),
        "POST",
        &format!("/fleet/tasks/{task_id}/cancel"),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let view =
        body_json(send(hub.clone(), "GET", &format!("/fleet/tasks/{task_id}"), None).await).await;
    let status = serde_json::from_value::<TransportStatus>(json!(view["status"])).unwrap();
    assert!(
        matches!(
            status,
            TransportStatus::Cancelled | TransportStatus::Succeeded
        ),
        "取消后应终态：{status:?}"
    );
}

/// 7) 幂等键：同 task_id 重复提交 → 409（无重复执行）。
#[tokio::test]
async fn duplicate_submit_rejected() {
    let (hub, _temp) = test_hub().await;
    let task_id = new_id("t");
    let body = submit_body(&task_id, "node-a", None);
    let first = send(hub.clone(), "POST", "/fleet/tasks/submit", Some(&body)).await;
    assert_eq!(first.status(), StatusCode::OK);
    let second = send(hub.clone(), "POST", "/fleet/tasks/submit", Some(&body)).await;
    assert_eq!(second.status(), StatusCode::CONFLICT, "重复提交应拒绝");
}

/// 8) 两节点冒烟链：注册→提交→租约→fencing→远程 step→审批→重放（无孤儿、无重复执行）。
#[tokio::test]
async fn two_node_smoke_chain() {
    let (hub, _temp) = test_hub().await;
    // 注册两个节点（持租约）。
    for node in ["node-a", "node-b"] {
        let resp = send(
            hub.clone(),
            "POST",
            "/fleet/nodes/register",
            Some(&register_body(node)),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }
    // 租约：node-a 持租约（epoch E1 + token T1）。
    let lease_a = hub.leases.lease("node-a").unwrap();
    let old_token = lease_a.token.clone();
    let old_epoch = lease_a.epoch;
    // 重注册 node-a（幂等心跳续租）：token/epoch 不变（续租不签发新 token）。
    let resp = send(
        hub.clone(),
        "POST",
        "/fleet/nodes/register",
        Some(&register_body("node-a")),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let renewed = hub.leases.lease("node-a").unwrap();
    assert_eq!(renewed.epoch, old_epoch, "续租不改变纪元");
    assert_eq!(renewed.token, old_token, "幂等重注册（心跳续租）不清 token");
    // fencing：重新 acquire（模拟失联重连）重新签发 token，旧 token 写被拒（防双写）。
    let re_acquire = hub.leases.acquire("node-a").unwrap();
    assert_eq!(re_acquire.epoch, old_epoch, "同 holder 重连 epoch 不变");
    assert_ne!(
        re_acquire.token, old_token,
        "重连重新签发 token（旧 token 作废）"
    );
    assert!(
        matches!(
            hub.leases.verify_write("node-a", &old_token, old_epoch),
            Err(owo_agent_core::lease::LeaseError::BadToken { .. })
        ),
        "旧 token 写应被 fencing 拒绝"
    );
    // 普通任务：node-b 执行（显式完成）。
    let t1 = new_id("t");
    send(
        hub.clone(),
        "POST",
        "/fleet/tasks/submit",
        Some(&submit_body(&t1, "node-b", None)),
    )
    .await;
    assert!(hub.transport.complete_task(&t1, true, json!("out-b")));
    wait_status(
        hub.clone(),
        &t1,
        TransportStatus::Succeeded,
        Duration::from_secs(5),
    )
    .await;
    // 远程 step（审批）：影响预览 + 结构化证据 → 批准 → 节点执行。
    let rs1 = new_id("rs");
    let input = approval_input(&rs1, "phone-1");
    send(
        hub.clone(),
        "POST",
        "/fleet/tasks/submit",
        Some(&submit_body(&rs1, "node-a", Some(input))),
    )
    .await;
    wait_status(
        hub.clone(),
        &rs1,
        TransportStatus::AwaitingApproval,
        Duration::from_secs(3),
    )
    .await;
    let resp = send(
        hub.clone(),
        "POST",
        &format!("/fleet/approvals/{rs1}/respond"),
        Some(&json!({ "decision": "approve", "approved_by": "owner" }).to_string()),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(hub.transport.complete_task(&rs1, true, json!("out-rs")));
    wait_status(
        hub.clone(),
        &rs1,
        TransportStatus::Succeeded,
        Duration::from_secs(5),
    )
    .await;
    // 重放：bus_store 已落盘节点状态/任务提交事件；幂等去重不重复。
    let msgs = hub.bus_store.replay_messages();
    assert!(
        msgs.iter()
            .any(|m| m.correlation_id.starts_with("node:status:")),
        "节点状态事件应落盘：{:?}",
        msgs.iter()
            .map(|m| m.correlation_id.clone())
            .collect::<Vec<_>>()
    );
    let deduped = owo_agent_core::fleet::dedupe_messages(&msgs);
    assert_eq!(deduped.len(), msgs.len(), "重放应无重复消息");
    // 无孤儿：全部任务已终态。
    let ids = hub.transport.task_ids();
    for id in ids {
        let status = hub.transport.task_status(&id).unwrap();
        assert!(
            matches!(
                status,
                TransportStatus::Succeeded | TransportStatus::Failed | TransportStatus::Cancelled
            ),
            "任务 {id} 不应挂起（无孤儿）：{status:?}"
        );
    }
}
