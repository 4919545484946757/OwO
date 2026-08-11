use chrono::Utc;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub ts: String,
    pub session_id: String,
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved: Option<bool>,
    pub detail: String,
}

/// M1 审计日志：进程内记录，后续接入 SQLite/文件。
#[derive(Debug, Default)]
pub struct AuditLog {
    pub entries: Vec<AuditEntry>,
}

impl AuditLog {
    pub fn record(
        &mut self,
        session_id: &str,
        event: &str,
        tool: Option<String>,
        approved: Option<bool>,
        detail: impl Into<String>,
    ) {
        self.entries.push(AuditEntry {
            ts: Utc::now().to_rfc3339(),
            session_id: session_id.to_string(),
            event: event.to_string(),
            tool,
            approved,
            detail: detail.into(),
        });
    }
}
