// R12:remote_step 完成，待主控接线
//! 远程步骤契约：perception/locate/act/verify 四类远程执行、审批回传所有者设备、结果带血缘。
//!
//! 设计来源：《多Agent并行体系-生产级设计与跨机扩展-2026-08-16.md》§2/§7：
//! - [`RemoteStepKind`]：perception/locate/act/verify（与 goal 步骤语义对齐）。
//! - **审批回传**：需要审批的远程步骤提交后产生 `ApprovalRequested` 事件，
//!   回传 `owner_device`；`approve` 产生 `ApprovalGranted` 并放行远端执行。
//! - **血缘**：输入/输出以 CAS 哈希引用，`lineage` 贯通父步骤/产物；
//!   结果经 `bus_store` 持久化、`experience_store` 记录。
//! - 执行经 [`crate::fleet_transport::FleetTransport`] 提交（`submit_via_transport`）。

use crate::cas_store::CasStore;
use crate::fleet_transport::{
    FleetTransport, TransportEvent, TransportEventKind, TransportStatus, TransportTask,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// 远程步骤类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteStepKind {
    Perception,
    Locate,
    Act,
    Verify,
}

/// 审批规格。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ApprovalSpec {
    /// 是否需要审批（默认 false）。
    pub required: bool,
    /// 审批回传设备（所有者设备标识）。
    #[serde(default)]
    pub owner_device: String,
    /// 审批展示摘要（不含敏感内容）。
    #[serde(default)]
    pub summary: String,
}

/// 结构化证据项（审批前展示；供所有者设备评估影响）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceItem {
    /// 证据类型（如 file_diff / command / network / input / output）。
    pub kind: String,
    /// 摘要（不含敏感内容）。
    pub summary: String,
    /// 细节（结构化 JSON 文本；缺省为空）。
    #[serde(default)]
    pub detail: String,
}

impl EvidenceItem {
    pub fn new(kind: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            summary: summary.into(),
            detail: String::new(),
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

/// 远程步骤定义（输入以 CAS 哈希引用）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteStep {
    pub step_id: String,
    pub kind: RemoteStepKind,
    /// 目标节点/worker（CapabilityCard 注册名）。
    pub worker: String,
    /// 输入产物哈希（CAS）。
    pub input_cas: String,
    pub correlation_id: String,
    #[serde(default)]
    pub approval: ApprovalSpec,
    /// 血缘：父步骤/产物引用。
    #[serde(default)]
    pub lineage: Vec<String>,
    /// 影响预览（审批前展示；如"修改 config.yaml、重启服务"）。
    #[serde(default)]
    pub impact_preview: String,
    /// 结构化证据（审批前展示；影响预览 + 证据齐备才批准执行）。
    #[serde(default)]
    pub evidence: Vec<EvidenceItem>,
}

impl RemoteStep {
    pub fn new(
        step_id: impl Into<String>,
        kind: RemoteStepKind,
        worker: impl Into<String>,
        correlation_id: impl Into<String>,
    ) -> Self {
        Self {
            step_id: step_id.into(),
            kind,
            worker: worker.into(),
            input_cas: String::new(),
            correlation_id: correlation_id.into(),
            approval: ApprovalSpec::default(),
            lineage: Vec::new(),
            impact_preview: String::new(),
            evidence: Vec::new(),
        }
    }

    pub fn with_input(mut self, cas: &CasStore, input: &[u8]) -> Result<Self, String> {
        self.input_cas = cas.put(input)?;
        Ok(self)
    }

    pub fn with_approval(mut self, approval: ApprovalSpec) -> Self {
        self.approval = approval;
        self
    }

    pub fn with_lineage(mut self, lineage: Vec<String>) -> Self {
        self.lineage = lineage;
        self
    }

    /// 设置影响预览（审批前展示）。
    pub fn with_impact_preview(mut self, preview: impl Into<String>) -> Self {
        self.impact_preview = preview.into();
        self
    }

    /// 追加结构化证据（审批前展示）。
    pub fn with_evidence(mut self, evidence: Vec<EvidenceItem>) -> Self {
        self.evidence = evidence;
        self
    }

    /// 审批材料是否齐备：影响预览 + 至少一条结构化证据（否则批准被拒）。
    pub fn approval_material_ready(&self) -> bool {
        !self.impact_preview.trim().is_empty() && !self.evidence.is_empty()
    }
}

/// 远程步骤结果（输出以 CAS 哈希引用，带血缘）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteStepOutcome {
    pub step_id: String,
    pub ok: bool,
    /// 输出产物哈希（CAS；失败时为空）。
    #[serde(default)]
    pub output_cas: String,
    /// 血缘（贯通父步骤）。
    #[serde(default)]
    pub lineage: Vec<String>,
    #[serde(default)]
    pub error: Option<String>,
}

impl RemoteStepOutcome {
    pub fn success(
        step_id: impl Into<String>,
        output_cas: impl Into<String>,
        lineage: Vec<String>,
    ) -> Self {
        Self {
            step_id: step_id.into(),
            ok: true,
            output_cas: output_cas.into(),
            lineage,
            error: None,
        }
    }

    pub fn failure(step_id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            step_id: step_id.into(),
            ok: false,
            output_cas: String::new(),
            lineage: Vec::new(),
            error: Some(error.into()),
        }
    }
}

/// 远程步骤生命周期事件（经 bus_store 持久化；审批回传所有者设备）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RemoteStepEvent {
    /// 提交远端（等待审批或执行）。
    Submitted {
        step_id: String,
        correlation_id: String,
        worker: String,
    },
    /// 审批请求回传所有者设备（含影响预览 + 结构化证据）。
    ApprovalRequested {
        step_id: String,
        owner_device: String,
        summary: String,
        correlation_id: String,
        /// 影响预览（审批前展示）。
        #[serde(default)]
        impact_preview: String,
        /// 结构化证据（审批前展示）。
        #[serde(default)]
        evidence: Vec<EvidenceItem>,
    },
    /// 审批通过（放行远端执行）。
    ApprovalGranted {
        step_id: String,
        approved_by: String,
        correlation_id: String,
    },
    /// 执行结果（带血缘）。
    Completed {
        outcome: RemoteStepOutcome,
        correlation_id: String,
    },
}

/// 通过传输提交远程步骤：审批类返回 `AwaitingApproval` 状态；执行类直接投递。
/// 输入/输出经 CAS 落盘（引用计数由调用方维护）。带等待超时（默认 60s），
/// 超时后 cancel 任务并报错（防远端任务孤儿/挂起）。
pub async fn submit_via_transport(
    transport: &Arc<dyn FleetTransport>,
    step: &RemoteStep,
    output_cas: &CasStore,
) -> Result<RemoteStepOutcome, String> {
    submit_via_transport_with_timeout(
        transport,
        step,
        output_cas,
        Some(DEFAULT_REMOTE_STEP_TIMEOUT),
    )
    .await
}

/// 远程步骤提交默认等待超时。
pub const DEFAULT_REMOTE_STEP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// 带超时的远程步骤提交（`None` = 不超时，仅受调用方预算兜底）。
pub async fn submit_via_transport_with_timeout(
    transport: &Arc<dyn FleetTransport>,
    step: &RemoteStep,
    output_cas: &CasStore,
    max_wait: Option<std::time::Duration>,
) -> Result<RemoteStepOutcome, String> {
    let task_id = format!("rs-{}", step.step_id);
    let mut task = TransportTask::new(
        task_id.clone(),
        step.worker.clone(),
        step.correlation_id.clone(),
        serde_json::json!({
            "kind": step.kind,
            "input_cas": step.input_cas,
            "approval": {
                "required": step.approval.required,
                "owner_device": step.approval.owner_device,
                "summary": step.approval.summary,
            },
            "impact_preview": step.impact_preview,
            "evidence": step.evidence,
        }),
    );
    task.lineage = step.lineage.clone();
    task.approval_required = step.approval.required;
    transport.submit(task).await?;
    // 轮询状态直至终态；审批事件由事件流回传；超时先 cancel 再报错（防孤儿/挂起）。
    let deadline = max_wait.map(|d| tokio::time::Instant::now() + d);
    loop {
        if let Some(deadline) = deadline {
            if tokio::time::Instant::now() >= deadline {
                let _ = transport.cancel(&task_id).await;
                return Err(format!("远程步骤 {task_id} 等待超时"));
            }
        }
        let status = transport.status(&task_id).await?;
        match status {
            TransportStatus::Succeeded => {
                let events = transport.events(&task_id).await?;
                let payload = events
                    .iter()
                    .find(|e| e.kind == TransportEventKind::Result)
                    .map(|e| e.payload.clone())
                    .unwrap_or(serde_json::Value::Null);
                let bytes = payload
                    .as_str()
                    .map(|s| s.as_bytes().to_vec())
                    .unwrap_or_default();
                let output_cas_hash = if bytes.is_empty() {
                    String::new()
                } else {
                    output_cas.put(&bytes)?
                };
                return Ok(RemoteStepOutcome::success(
                    step.step_id.clone(),
                    output_cas_hash,
                    step.lineage.clone(),
                ));
            }
            TransportStatus::Failed => {
                let events = transport.events(&task_id).await?;
                let error = events
                    .iter()
                    .find(|e| e.kind == TransportEventKind::Cancelled)
                    .and_then(|e| e.payload.as_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| "transport 任务失败".to_string());
                return Err(error);
            }
            TransportStatus::AwaitingApproval => {
                // 审批未决：回传事件已挂在事件流（bus_store 持久化由调用方接入）。
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            TransportStatus::Cancelled => {
                return Err("远程步骤被取消".to_string());
            }
            _ => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
        }
    }
}

/// 审批事件 → 传输事件（回传所有者设备；含影响预览 + 结构化证据；经 bus_store 落盘由调用方接入）。
pub fn approval_request_event(step: &RemoteStep) -> TransportEvent {
    TransportEvent {
        task_id: format!("rs-{}", step.step_id),
        kind: TransportEventKind::ApprovalRequested,
        correlation_id: step.correlation_id.clone(),
        payload: serde_json::json!({
            "owner_device": step.approval.owner_device,
            "summary": step.approval.summary,
            "impact_preview": step.impact_preview,
            "evidence": step.evidence,
        }),
        lineage: step.lineage.clone(),
    }
}

/// 审批通过：向传输层放行（`AwaitingApproval` → 执行；不支持的传输显式拒绝，不静默降级）。
pub async fn approve_transport_task(
    transport: &Arc<dyn FleetTransport>,
    task_id: &str,
    approved_by: &str,
) -> Result<(), String> {
    let status = transport.status(task_id).await?;
    if matches!(
        status,
        TransportStatus::Succeeded | TransportStatus::Failed | TransportStatus::Cancelled
    ) {
        return Err(format!("任务 {task_id} 已终态，无法审批"));
    }
    transport.approve(task_id, approved_by).await
}
