//! Agent SDK 公开线协议（v1 契约，HTTP JSON + SSE 事件）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    pub workspace: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub workspace: String,
    pub model: String,
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub fork_point: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRequest {
    pub prompt: String,
    /// 附件 ID（由 `POST /session/{id}/attachments` 返回；发送时注入路径上下文）。
    #[serde(default)]
    pub attachments: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkRequest {
    pub message_index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewindRequest {
    pub keep: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalRunRequest {
    pub suite_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionResponse {
    pub allow: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remember: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub healthy: bool,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SseEvent {
    Progress {
        message: String,
    },
    ToolUse {
        id: String,
        tool: String,
        args: Value,
    },
    ToolResult {
        id: String,
        tool: String,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    PermissionRequest {
        request_id: String,
        tool: String,
        args: Value,
        reason: String,
    },
    Final {
        text: String,
    },
    TokenDelta {
        delta: String,
    },
    Compaction {
        summary: String,
    },
}
