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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRequest {
    pub prompt: String,
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
