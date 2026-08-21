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
    /// 用户可编辑的会话标题（None 时按首条用户消息自动显示）。
    #[serde(default)]
    pub title: Option<String>,
    /// 归档标记（默认列表可隐藏）。
    #[serde(default)]
    pub archived: bool,
    /// 置顶标记（列表优先）。
    #[serde(default)]
    pub pinned: bool,
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
            title: None,
            archived: false,
            pinned: false,
        }
    }

    /// 展示标题：优先自定义标题，否则取首条用户消息，最后回退为会话短 ID。
    pub fn display_title(&self) -> String {
        if let Some(title) = &self.title {
            let trimmed = title.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        if let Some(first) = self
            .messages
            .iter()
            .find(|message| message.role == "user")
            .and_then(|message| message.content.as_deref())
        {
            let trimmed = first.trim();
            if !trimmed.is_empty() {
                return trimmed.chars().take(40).collect();
            }
        }
        let short_id: String = self.id.chars().take(8).collect();
        format!("会话 {short_id}")
    }

    pub fn rename(&mut self, title: String) {
        self.title = Some(title);
        self.updated_at = Utc::now().to_rfc3339();
    }

    pub fn set_archived(&mut self, archived: bool) {
        self.archived = archived;
        self.updated_at = Utc::now().to_rfc3339();
    }

    pub fn set_pinned(&mut self, pinned: bool) {
        self.pinned = pinned;
        self.updated_at = Utc::now().to_rfc3339();
    }

    pub fn push(&mut self, message: ChatMessage) {
        self.messages.push(message);
        self.redo_stack.clear();
        self.message_redo_stack.clear();
        self.updated_at = Utc::now().to_rfc3339();
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
        let messages = if self.messages.is_empty() {
            Vec::new()
        } else {
            let end = message_index.min(self.messages.len() - 1);
            self.messages[..=end].to_vec()
        };
        let now = Utc::now().to_rfc3339();
        Session {
            id: uuid::Uuid::new_v4().to_string(),
            workspace: self.workspace.clone(),
            model: self.model.clone(),
            system_prompt: self.system_prompt.clone(),
            messages,
            snapshots: HashMap::new(),
            created_at: now.clone(),
            updated_at: now,
            parent_id: Some(self.id.clone()),
            fork_point: Some(message_index),
            redo_stack: Vec::new(),
            message_redo_stack: Vec::new(),
            title: None,
            archived: false,
            pinned: false,
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
    /// 持久化审计记录（默认 no-op；SQLite 存储落库）。
    fn append_audit(&self, entries: &[crate::audit::AuditEntry]) -> Result<(), AgentError> {
        let _ = entries;
        Ok(())
    }
    /// 最近 N 条审计记录（默认空；SQLite 存储返回持久化记录）。
    fn recent_audit(&self, limit: usize) -> Vec<crate::audit::AuditEntry> {
        let _ = limit;
        Vec::new()
    }

    /// 分页/过滤/搜索审计（默认退化为 recent_audit；SQLite 存储提供完整实现）。
    fn query_audit(
        &self,
        query: &crate::sqlite_store::AuditQuery,
    ) -> (Vec<crate::audit::AuditEntry>, usize) {
        let entries = self.recent_audit(query.limit.max(1));
        let total = entries.len();
        (entries, total)
    }

    /// 清空会话与审计（R8 存储运维；默认不支持，SQLite 存储提供实现）。
    fn clear(&self) -> Result<(), AgentError> {
        Err(AgentError::Session("当前存储不支持清空".into()))
    }

    /// 存储是否处于只读降级（R8：迁移失败后的安全状态；默认否）。
    fn is_read_only(&self) -> bool {
        false
    }

    /// 只读降级原因/迁移警告（R8；默认无）。
    fn migration_warning(&self) -> Option<String> {
        None
    }
}

/// M1 会话存储：JSON 文件（后续迁移 SQLite）。
/// R9：可选加密模式——落盘经 storage_crypto 文件信封加密（`<id>.json.owo-crypt`），
/// 读取优先解密；明文 `.json` 保持兼容（既有存储不受影响）。
pub struct JsonSessionStore {
    root: PathBuf,
    encrypted: bool,
}

impl JsonSessionStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            encrypted: false,
        }
    }

    /// 加密模式：会话落盘经 DPAPI 信封加密（非 Windows 下 save 会显式失败）。
    pub fn new_encrypted(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            encrypted: true,
        }
    }

    fn plain_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.json"))
    }

    fn encrypted_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.json.owo-crypt"))
    }

    fn path(&self, id: &str) -> PathBuf {
        if self.encrypted {
            self.encrypted_path(id)
        } else {
            self.plain_path(id)
        }
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
        // 加密形态优先，明文兜底（读取解密；加密文件损坏 → 显式错误，不静默回退明文）。
        let encrypted_path = self.encrypted_path(id);
        if encrypted_path.exists() {
            let content = crate::storage_crypto::decrypt_file_envelope(&encrypted_path)
                .map_err(|error| AgentError::Session(format!("会话 {id} 解密失败：{error}")))?;
            return Ok(serde_json::from_slice(&content)?);
        }
        let content = std::fs::read_to_string(self.plain_path(id))
            .map_err(|e| AgentError::Session(format!("会话 {id} 读取失败：{e}")))?;
        Ok(serde_json::from_str(&content)?)
    }

    fn save(&self, session: &Session) -> Result<(), AgentError> {
        std::fs::create_dir_all(&self.root)?;
        let target = self.path(&session.id);
        let content = serde_json::to_vec_pretty(session)?;
        if self.encrypted {
            crate::storage_crypto::encrypt_file_envelope(&target, &content).map_err(|error| {
                AgentError::Session(format!("会话 {} 落盘加密失败：{error}", session.id))
            })?;
            return Ok(());
        }
        let tmp = self.root.join(format!("{}.tmp", session.id));
        std::fs::write(&tmp, content)?;
        if let Err(rename_error) = std::fs::rename(&tmp, &target) {
            // Windows 不允许 rename 覆盖已有文件；保留临时文件写入语义，
            // 在目标存在时执行一次兼容替换。
            if !target.exists() {
                return Err(rename_error.into());
            }
            std::fs::remove_file(&target)?;
            std::fs::rename(&tmp, &target)?;
        }
        Ok(())
    }

    fn list(&self) -> Vec<String> {
        let mut sessions = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if let Some(id) = name.strip_suffix(".json.owo-crypt") {
                    sessions.push(id.to_string());
                } else if let Some(id) = name.strip_suffix(".json") {
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
        store.save(&session).unwrap();
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
    fn fork_on_empty_session_does_not_panic() {
        let session = Session::new(".", "mock", None);
        let child = session.fork(999999);
        assert!(child.messages.is_empty());
        assert_eq!(child.parent_id.as_deref(), Some(session.id.as_str()));
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

    #[tokio::test]
    async fn rewind_and_revert_restores_files_before_truncating_history() {
        let workspace =
            std::env::temp_dir().join(format!("owo-session-rewind-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();
        let path = workspace.join("changed.txt");
        std::fs::write(&path, "after").unwrap();

        let mut session = Session::new(&workspace, "mock", None);
        session.push(ChatMessage::user("first".to_string()));
        session.push(ChatMessage::assistant_text("reply".to_string()));
        session.snapshots.insert(
            path.to_string_lossy().replace('\\', "/"),
            SnapshotEntry {
                original_b64: Some(BASE64.encode("before")),
            },
        );

        session.revert().await.unwrap();
        let removed = session.rewind(1);

        assert_eq!(removed.len(), 1);
        assert_eq!(session.messages.len(), 1);
        assert!(session.snapshots.is_empty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "before");
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[tokio::test]
    async fn rewind_does_not_change_files_when_keep_is_current_length() {
        let workspace =
            std::env::temp_dir().join(format!("owo-session-rewind-noop-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();
        let path = workspace.join("changed.txt");
        std::fs::write(&path, "after").unwrap();

        let mut session = Session::new(&workspace, "mock", None);
        session.push(ChatMessage::user("first".to_string()));
        session.snapshots.insert(
            path.to_string_lossy().replace('\\', "/"),
            SnapshotEntry {
                original_b64: Some(BASE64.encode("before")),
            },
        );

        let removed = session.rewind(1);

        assert!(removed.is_empty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "after");
        assert!(!session.snapshots.is_empty());
        let _ = std::fs::remove_dir_all(&workspace);
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

    #[test]
    fn pushing_new_history_invalidates_both_redo_stacks() {
        let mut session = Session::new(".", "mock", None);
        for index in 0..3 {
            session.push(ChatMessage::user(format!("m{index}")));
        }
        session.rewind(1);
        session.undo_message(1);

        session.push(ChatMessage::user("new branch".to_string()));

        assert!(session.redo().is_none());
        assert!(session.redo_message().is_none());
    }

    #[test]
    fn title_archive_pin_round_trip() {
        let mut session = Session::new(".", "mock", None);
        session.push(ChatMessage::user("给 parseConfig 补测试".to_string()));
        assert_eq!(session.display_title(), "给 parseConfig 补测试");
        session.rename("我的任务".to_string());
        assert_eq!(session.display_title(), "我的任务");
        session.set_pinned(true);
        session.set_archived(true);
        assert!(session.pinned);
        assert!(session.archived);
        let child = session.fork(0);
        assert!(child.title.is_none());
        assert!(!child.pinned);
        assert!(!child.archived);
        assert_eq!(child.display_title(), "给 parseConfig 补测试");
    }
}
