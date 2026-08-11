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
    /// None 表示文件原本不存在（回滚时删除）。
    #[serde(default)]
    pub original_b64: Option<String>,
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
    /// 父会话（由 fork 产生时）。
    #[serde(default)]
    pub parent_id: Option<String>,
    /// fork 时的消息下标（含）。
    #[serde(default)]
    pub fork_point: Option<usize>,
    /// 被 /rewind 截断的历史，可 /redo 恢复。
    #[serde(default)]
    pub redo_stack: Vec<Vec<ChatMessage>>,
    /// 被 /undo-msg 移除的消息，可 /redo-msg 恢复。
    #[serde(default)]
    pub message_redo_stack: Vec<Vec<ChatMessage>>,
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
            parent_id: None,
            fork_point: None,
            redo_stack: Vec::new(),
            message_redo_stack: Vec::new(),
        }
    }

    pub fn push(&mut self, message: ChatMessage) {
        self.messages.push(message);
    }

    /// 当前会话改动 diff（相对工作区路径）。
    pub fn diff(&self) -> Vec<FileDiff> {
        let mut diffs = Vec::new();
        for (path, snapshot) in &self.snapshots {
            let original = snapshot.original_b64.as_ref().and_then(|encoded| {
                BASE64
                    .decode(encoded)
                    .ok()
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            });
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
            let target = PathBuf::from(path);
            match &snapshot.original_b64 {
                Some(encoded) => {
                    let bytes = BASE64
                        .decode(encoded)
                        .map_err(|e| AgentError::Session(format!("快照解码失败：{e}")))?;
                    if let Some(parent) = target.parent() {
                        tokio::fs::create_dir_all(parent).await?;
                    }
                    tokio::fs::write(&target, bytes).await?;
                }
                None => {
                    let _ = tokio::fs::remove_file(&target).await;
                }
            }
            restored.push(relative_display(&self.workspace, &target));
        }
        self.snapshots.clear();
        self.updated_at = Utc::now().to_rfc3339();
        Ok(restored)
    }

    /// 在指定消息处派生一个子会话（继承历史，快照与 redo 栈清空）。
    pub fn fork(&self, message_index: usize) -> Session {
        let end = message_index.min(self.messages.len().saturating_sub(1));
        let now = Utc::now().to_rfc3339();
        Session {
            id: uuid::Uuid::new_v4().to_string(),
            workspace: self.workspace.clone(),
            model: self.model.clone(),
            system_prompt: self.system_prompt.clone(),
            messages: self.messages[..=end].to_vec(),
            snapshots: HashMap::new(),
            created_at: now.clone(),
            updated_at: now,
            parent_id: Some(self.id.clone()),
            fork_point: Some(end),
            redo_stack: Vec::new(),
            message_redo_stack: Vec::new(),
        }
    }

    /// 回退到仅保留前 `keep` 条消息，同时清空文件快照；返回被移除的历史。
    pub fn rewind(&mut self, keep: usize) -> Vec<ChatMessage> {
        if keep >= self.messages.len() {
            return Vec::new();
        }
        let removed = self.messages.split_off(keep);
        self.redo_stack.push(removed.clone());
        self.snapshots.clear();
        self.updated_at = Utc::now().to_rfc3339();
        removed
    }

    /// 恢复最近一次 rewind 移除的历史。
    pub fn redo(&mut self) -> Option<Vec<ChatMessage>> {
        let tail = self.redo_stack.pop()?;
        self.messages.extend(tail.iter().cloned());
        self.updated_at = Utc::now().to_rfc3339();
        Some(tail)
    }

    /// 移除最近 `count` 条消息（消息级撤销），压入可恢复栈。
    pub fn undo_message(&mut self, count: usize) -> Option<Vec<ChatMessage>> {
        let count = count.min(self.messages.len());
        if count == 0 {
            return None;
        }
        let split_at = self.messages.len() - count;
        let removed = self.messages.split_off(split_at);
        self.message_redo_stack.push(removed.clone());
        self.updated_at = Utc::now().to_rfc3339();
        Some(removed)
    }

    /// 恢复最近一次消息级撤销。
    pub fn redo_message(&mut self) -> Option<Vec<ChatMessage>> {
        let tail = self.message_redo_stack.pop()?;
        self.messages.extend(tail.iter().cloned());
        self.updated_at = Utc::now().to_rfc3339();
        Some(tail)
    }
}

fn relative_display(workspace: &Path, path: &Path) -> String {
    let normalize = |value: &Path| -> String {
        let raw = value.to_string_lossy().replace('\\', "/");
        raw.strip_prefix("//?/").unwrap_or(&raw).to_string()
    };
    let workspace = normalize(workspace);
    let path = normalize(path);
    path.strip_prefix(&workspace)
        .map(|relative| relative.trim_start_matches('/').to_string())
        .unwrap_or(path)
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
    /// 列出全部会话 ID（按更新时间倒序）。
    fn list(&self) -> Vec<String> {
        Vec::new()
    }
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

    fn list(&self) -> Vec<String> {
        let mut sessions = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if let Some(id) = name.strip_suffix(".json") {
                    sessions.push(id.to_string());
                }
            }
        }
        sessions.sort_by(|a, b| {
            let ta = self
                .load(a)
                .map(|s| s.updated_at.clone())
                .unwrap_or_default();
            let tb = self
                .load(b)
                .map(|s| s.updated_at.clone())
                .unwrap_or_default();
            tb.cmp(&ta)
        });
        sessions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::ChatMessage;

    #[test]
    fn session_store_round_trip_and_list() {
        let root =
            std::env::temp_dir().join(format!("owo-session-store-test-{}", uuid::Uuid::new_v4()));
        let store = JsonSessionStore::new(&root);
        let session = store
            .create(std::path::Path::new("."), "mock", None)
            .unwrap();
        assert_eq!(store.list().len(), 1);
        let loaded = store.load(&session.id).unwrap();
        assert_eq!(loaded.id, session.id);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fork_creates_child_with_history() {
        let mut session = Session::new(".", "mock", None);
        session.push(ChatMessage::user("a".to_string()));
        session.push(ChatMessage::assistant_text("b".to_string()));
        session.push(ChatMessage::user("c".to_string()));

        let child = session.fork(1);
        assert_eq!(child.messages.len(), 2);
        assert_eq!(child.parent_id.as_deref(), Some(session.id.as_str()));
        assert_eq!(child.fork_point, Some(1));
        assert!(child.snapshots.is_empty());
        assert!(child.redo_stack.is_empty());
    }

    #[test]
    fn rewind_and_redo_round_trip() {
        let mut session = Session::new(".", "mock", None);
        for index in 0..5 {
            session.push(ChatMessage::user(format!("m{index}")));
        }
        let removed = session.rewind(2);
        assert_eq!(removed.len(), 3);
        assert_eq!(session.messages.len(), 2);

        let restored = session.redo().expect("存在可恢复历史");
        assert_eq!(restored.len(), 3);
        assert_eq!(session.messages.len(), 5);
        assert!(session.redo().is_none());
    }

    #[test]
    fn message_undo_and_redo_round_trip() {
        let mut session = Session::new(".", "mock", None);
        for index in 0..4 {
            session.push(ChatMessage::user(format!("m{index}")));
        }
        let removed = session.undo_message(2).expect("存在可撤销消息");
        assert_eq!(removed.len(), 2);
        assert_eq!(session.messages.len(), 2);
        assert!(session.undo_message(0).is_none());

        let restored = session.redo_message().expect("存在可恢复消息");
        assert_eq!(restored.len(), 2);
        assert_eq!(session.messages.len(), 4);
        assert!(session.redo_message().is_none());
    }
}
