use crate::error::AgentError;
use crate::gateway::ChatMessage;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use chrono::Utc;
use owo_agent_protocol::FileDiff;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub original_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub workspace: PathBuf,
    pub model: String,
    pub system_prompt: Option<String>,
    pub messages: Vec<ChatMessage>,
    pub snapshots: HashMap<String, SnapshotEntry>,
    pub created_at: String,
    pub updated_at: String,
}

impl Session {
    pub fn new(
        workspace: impl Into<PathBuf>,
        model: impl Into<String>,
        system_prompt: Option<String>,
    ) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            workspace: workspace.into(),
            model: model.into(),
            system_prompt,
            messages: Vec::new(),
            snapshots: HashMap::new(),
            created_at: now.clone(),
            updated_at: now,
        }
    }

    pub fn push(&mut self, message: ChatMessage) {
        self.messages.push(message);
    }

    /// 当前会话改动 diff（相对工作区路径）。
    pub fn diff(&self) -> Vec<FileDiff> {
        let mut diffs = Vec::new();
        for (path, snapshot) in &self.snapshots {
            let original = BASE64
                .decode(&snapshot.original_b64)
                .ok()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
            let current = std::fs::read(path)
                .ok()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
            if original == current {
                continue;
            }
            diffs.push(FileDiff {
                path: relative_display(&self.workspace, Path::new(path)),
                before: original,
                after: current,
            });
        }
        diffs
    }

    /// 回滚全部已快照的写操作，返回被恢复的路径。
    pub async fn revert(&mut self) -> Result<Vec<String>, AgentError> {
        let mut restored = Vec::new();
        for (path, snapshot) in &self.snapshots {
            let bytes = BASE64
                .decode(&snapshot.original_b64)
                .map_err(|e| AgentError::Session(format!("快照解码失败：{e}")))?;
            let target = PathBuf::from(path);
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&target, bytes).await?;
            restored.push(relative_display(&self.workspace, &target));
        }
        self.snapshots.clear();
        self.updated_at = Utc::now().to_rfc3339();
        Ok(restored)
    }
}

fn relative_display(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub trait SessionStore: Send + Sync {
    fn create(
        &self,
        workspace: &Path,
        model: &str,
        system_prompt: Option<&str>,
    ) -> Result<Session, AgentError>;
    fn load(&self, id: &str) -> Result<Session, AgentError>;
    fn save(&self, session: &Session) -> Result<(), AgentError>;
}

/// M1 会话存储：JSON 文件（后续迁移 SQLite）。
pub struct JsonSessionStore {
    root: PathBuf,
}

impl JsonSessionStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.json"))
    }
}

impl SessionStore for JsonSessionStore {
    fn create(
        &self,
        workspace: &Path,
        model: &str,
        system_prompt: Option<&str>,
    ) -> Result<Session, AgentError> {
        let session = Session::new(workspace, model, system_prompt.map(str::to_string));
        self.save(&session)?;
        Ok(session)
    }

    fn load(&self, id: &str) -> Result<Session, AgentError> {
        let content = std::fs::read_to_string(self.path(id))
            .map_err(|e| AgentError::Session(format!("会话 {id} 读取失败：{e}")))?;
        Ok(serde_json::from_str(&content)?)
    }

    fn save(&self, session: &Session) -> Result<(), AgentError> {
        std::fs::create_dir_all(&self.root)?;
        let tmp = self.root.join(format!("{}.tmp", session.id));
        let content = serde_json::to_vec_pretty(session)?;
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, self.path(&session.id))?;
        Ok(())
    }
}
