//! Blackboard：Policy 门控的共享工作区状态（多 Agent P0）。
//!
//! 设计来源：《多Agent并行体系-生产级设计与跨机扩展-2026-08-16.md》§5.4。
//! - **单写主**：只有唯一 writer 可写/删/转移；其余 agent 只读（禁止自由并发写）。
//! - **Policy 门控**：读走 Level::Read（始终允许）；写要求策略非只读，否则拒绝。
//! - **事件溯源**：每次变更 append 到事件日志（seq 单调递增），可审计、可回放、可恢复。

use crate::permissions::{Decision, Level, PermissionRequest, Policy};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use tokio::sync::RwLock;

/// 黑板错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlackboardError {
    NotWriter { actor: String, writer: String },
    PolicyDenied(String),
    KeyNotFound(String),
    Inconsistent(String),
}

impl fmt::Display for BlackboardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotWriter { actor, writer } => {
                write!(f, "非写主 {actor} 尝试写黑板（当前写主 {writer}）")
            }
            Self::PolicyDenied(reason) => write!(f, "策略拒绝：{reason}"),
            Self::KeyNotFound(key) => write!(f, "黑板键不存在：{key}"),
            Self::Inconsistent(reason) => write!(f, "黑板状态不一致：{reason}"),
        }
    }
}

impl Error for BlackboardError {}

/// 黑板操作类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlackboardOp {
    Set,
    Delete,
    Transfer,
}

/// 事件溯源条目（append-only）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlackboardEvent {
    pub seq: u64,
    pub actor: String,
    pub key: String,
    pub op: BlackboardOp,
    /// Set：新值；Transfer：{"to": 新写主}；Delete：无。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    pub at: String,
}

/// 黑板条目（带版本号，写操作可见的"写后读"一致性）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlackboardEntry {
    pub key: String,
    pub value: serde_json::Value,
    pub version: u64,
    pub writer: String,
    pub updated_at: String,
}

/// 可持久化快照（restart 恢复）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlackboardSnapshot {
    pub owner: String,
    pub writer: String,
    pub entries: Vec<BlackboardEntry>,
    pub events: Vec<BlackboardEvent>,
    pub seq: u64,
}

/// 共享黑板：单写主 + Policy 门控 + 事件溯源。
#[derive(Clone)]
pub struct Blackboard {
    inner: Arc<RwLock<BlackboardInner>>,
}

struct BlackboardInner {
    owner: String,
    writer: String,
    policy: Arc<Policy>,
    entries: BTreeMap<String, BlackboardEntry>,
    events: Vec<BlackboardEvent>,
    seq: u64,
}

impl Blackboard {
    /// 新建黑板：owner 即初始写主（调度主）。
    pub fn new(owner: impl Into<String>, policy: Policy) -> Self {
        let owner = owner.into();
        Self {
            inner: Arc::new(RwLock::new(BlackboardInner {
                writer: owner.clone(),
                owner,
                policy: Arc::new(policy),
                entries: BTreeMap::new(),
                events: Vec::new(),
                seq: 0,
            })),
        }
    }

    /// 门控读：Level::Read 恒允许；KeyNotFound 返回错误。
    pub async fn read(&self, key: &str) -> Result<serde_json::Value, BlackboardError> {
        let inner = self.inner.read().await;
        let req = PermissionRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            tool: "blackboard.read".to_string(),
            args: serde_json::json!({ "key": key }),
            level: Level::Read,
            reason: format!("blackboard 读取 {key}"),
        };
        if inner.policy.decision(&req) != Decision::Allow {
            return Err(BlackboardError::PolicyDenied(format!(
                "blackboard 读取 {key} 被策略拒绝"
            )));
        }
        inner
            .entries
            .get(key)
            .map(|e| e.value.clone())
            .ok_or_else(|| BlackboardError::KeyNotFound(key.to_string()))
    }

    /// 读取为字符串（Value::String 原样；其他类型 JSON 序列化）。
    pub async fn read_as_string(&self, key: &str) -> Result<String, BlackboardError> {
        match self.read(key).await? {
            serde_json::Value::String(s) => Ok(s),
            other => Ok(serde_json::to_string(&other).unwrap_or_default()),
        }
    }

    /// 写（仅当前写主；策略非只读）。返回新条目（版本 +1）。
    pub async fn write(
        &self,
        actor: &str,
        key: &str,
        value: serde_json::Value,
    ) -> Result<BlackboardEntry, BlackboardError> {
        let mut inner = self.inner.write().await;
        if actor != inner.writer {
            return Err(BlackboardError::NotWriter {
                actor: actor.to_string(),
                writer: inner.writer.clone(),
            });
        }
        // Policy 门控：只读策略下黑板不可写（写 = Level::Write 语义，无审批流则整体拒绝）。
        if inner.policy.is_read_only() {
            return Err(BlackboardError::PolicyDenied(format!(
                "blackboard 写入 {key} 被策略拒绝（只读策略下黑板不可写）"
            )));
        }
        let version = inner
            .entries
            .get(key)
            .map(|e| e.version.saturating_add(1))
            .unwrap_or(1);
        let entry = BlackboardEntry {
            key: key.to_string(),
            value,
            version,
            writer: actor.to_string(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };
        inner.seq = inner.seq.saturating_add(1);
        let seq = inner.seq;
        inner.events.push(BlackboardEvent {
            seq,
            actor: actor.to_string(),
            key: key.to_string(),
            op: BlackboardOp::Set,
            value: Some(entry.value.clone()),
            at: chrono::Utc::now().to_rfc3339(),
        });
        inner.entries.insert(key.to_string(), entry.clone());
        Ok(entry)
    }

    /// 删除（仅当前写主；策略非只读）。返回被删除条目。
    pub async fn delete(&self, actor: &str, key: &str) -> Result<BlackboardEntry, BlackboardError> {
        let mut inner = self.inner.write().await;
        if actor != inner.writer {
            return Err(BlackboardError::NotWriter {
                actor: actor.to_string(),
                writer: inner.writer.clone(),
            });
        }
        if inner.policy.is_read_only() {
            return Err(BlackboardError::PolicyDenied(
                "blackboard 删除在只读策略下被拒绝".to_string(),
            ));
        }
        let removed = inner
            .entries
            .remove(key)
            .ok_or_else(|| BlackboardError::KeyNotFound(key.to_string()))?;
        inner.seq = inner.seq.saturating_add(1);
        let seq = inner.seq;
        inner.events.push(BlackboardEvent {
            seq,
            actor: actor.to_string(),
            key: key.to_string(),
            op: BlackboardOp::Delete,
            value: None,
            at: chrono::Utc::now().to_rfc3339(),
        });
        Ok(removed)
    }

    /// 写主转移（fencing 语义：仅当前写主可转移；目标必须非空）。
    pub async fn transfer_writer(&self, from: &str, to: &str) -> Result<(), BlackboardError> {
        if to.is_empty() {
            return Err(BlackboardError::Inconsistent("新写主不能为空".to_string()));
        }
        let mut inner = self.inner.write().await;
        if from != inner.writer {
            return Err(BlackboardError::NotWriter {
                actor: from.to_string(),
                writer: inner.writer.clone(),
            });
        }
        inner.writer = to.to_string();
        inner.seq = inner.seq.saturating_add(1);
        let seq = inner.seq;
        inner.events.push(BlackboardEvent {
            seq,
            actor: from.to_string(),
            key: "writer".to_string(),
            op: BlackboardOp::Transfer,
            value: Some(serde_json::json!({ "to": to })),
            at: chrono::Utc::now().to_rfc3339(),
        });
        Ok(())
    }

    pub async fn writer(&self) -> String {
        self.inner.read().await.writer.clone()
    }

    pub async fn owner(&self) -> String {
        self.inner.read().await.owner.clone()
    }

    pub async fn get(&self, key: &str) -> Result<BlackboardEntry, BlackboardError> {
        self.inner
            .read()
            .await
            .entries
            .get(key)
            .cloned()
            .ok_or_else(|| BlackboardError::KeyNotFound(key.to_string()))
    }

    pub async fn entries(&self) -> Vec<BlackboardEntry> {
        self.inner.read().await.entries.values().cloned().collect()
    }

    pub async fn len(&self) -> usize {
        self.inner.read().await.entries.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }

    /// 事件日志（append-only 审计视图）。
    pub async fn events(&self) -> Vec<BlackboardEvent> {
        self.inner.read().await.events.clone()
    }

    /// 一致性快照（持久化/恢复）。
    pub async fn snapshot(&self) -> BlackboardSnapshot {
        let inner = self.inner.read().await;
        BlackboardSnapshot {
            owner: inner.owner.clone(),
            writer: inner.writer.clone(),
            entries: inner.entries.values().cloned().collect(),
            events: inner.events.clone(),
            seq: inner.seq,
        }
    }

    /// 从快照恢复；校验事件 seq 单调 + 写主已声明。
    pub fn from_snapshot(snapshot: BlackboardSnapshot, policy: Policy) -> Result<Self, String> {
        if snapshot.writer.is_empty() {
            return Err("快照缺少写主声明".to_string());
        }
        let mut prev = 0u64;
        for event in &snapshot.events {
            if event.seq <= prev {
                return Err(format!("事件日志 seq 不单调：{} 在 {} 后", event.seq, prev));
            }
            prev = event.seq;
        }
        if prev != snapshot.seq {
            return Err(format!(
                "事件日志不完整：seq={} 与快照 seq={} 不一致",
                prev, snapshot.seq
            ));
        }
        let mut entries = BTreeMap::new();
        for entry in snapshot.entries {
            entries.insert(entry.key.clone(), entry);
        }
        Ok(Self {
            inner: Arc::new(RwLock::new(BlackboardInner {
                owner: snapshot.owner,
                writer: snapshot.writer,
                policy: Arc::new(policy),
                entries,
                events: snapshot.events,
                seq: snapshot.seq,
            })),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn writable_blackboard() -> Blackboard {
        Blackboard::new("runner-a", Policy::new("."))
    }

    #[tokio::test]
    async fn single_writer_enforced() {
        let bb = writable_blackboard();
        bb.write("runner-a", "k1", json!("v1")).await.unwrap();
        assert_eq!(bb.read("k1").await.unwrap(), json!("v1"));
        let err = bb.write("worker-b", "k1", json!("x")).await.unwrap_err();
        assert_eq!(
            err,
            BlackboardError::NotWriter {
                actor: "worker-b".to_string(),
                writer: "runner-a".to_string()
            }
        );
        assert_eq!(
            bb.read("k1").await.unwrap(),
            json!("v1"),
            "非写主写入被拒，值不变"
        );
    }

    #[tokio::test]
    async fn read_only_policy_gates_writes() {
        let bb = Blackboard::new("runner-a", Policy::read_only("."));
        let err = bb.write("runner-a", "k1", json!("v")).await.unwrap_err();
        assert!(matches!(err, BlackboardError::PolicyDenied(_)));
        let bb2 = writable_blackboard();
        bb2.write("runner-a", "k2", json!("v2")).await.unwrap();
        assert_eq!(bb2.read("k2").await.unwrap(), json!("v2"));
    }

    #[tokio::test]
    async fn write_creates_versioned_entries_and_events() {
        let bb = writable_blackboard();
        let e1 = bb.write("runner-a", "k1", json!("v1")).await.unwrap();
        assert_eq!(e1.version, 1);
        let e2 = bb.write("runner-a", "k1", json!("v2")).await.unwrap();
        assert_eq!(e2.version, 2);
        let events = bb.events().await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 1);
        assert_eq!(events[1].seq, 2);
        assert_eq!(events[0].op, BlackboardOp::Set);
        assert_eq!(events[0].actor, "runner-a");
    }

    #[tokio::test]
    async fn transfer_writer_only_by_current_writer() {
        let bb = writable_blackboard();
        assert_eq!(bb.writer().await, "runner-a");
        bb.transfer_writer("runner-a", "runner-b").await.unwrap();
        assert_eq!(bb.writer().await, "runner-b");
        let err = bb.write("runner-a", "k1", json!("x")).await.unwrap_err();
        assert!(matches!(err, BlackboardError::NotWriter { .. }));
        bb.write("runner-b", "k1", json!("v1")).await.unwrap();
        assert_eq!(bb.read("k1").await.unwrap(), json!("v1"));
        let events = bb.events().await;
        assert!(events.iter().any(|e| e.op == BlackboardOp::Transfer));
    }

    #[tokio::test]
    async fn snapshot_roundtrip_restores_state() {
        let bb = writable_blackboard();
        bb.write("runner-a", "k1", json!("v1")).await.unwrap();
        bb.write("runner-a", "k2", json!({"n": 2})).await.unwrap();
        let snap = bb.snapshot().await;
        let restored = Blackboard::from_snapshot(snap.clone(), Policy::new(".")).unwrap();
        assert_eq!(restored.read("k1").await.unwrap(), json!("v1"));
        assert_eq!(restored.read("k2").await.unwrap(), json!({"n": 2}));
        assert_eq!(restored.writer().await, snap.writer);
        assert_eq!(restored.entries().await.len(), 2);
    }

    #[tokio::test]
    async fn snapshot_rejects_tampered_event_log() {
        let bb = writable_blackboard();
        bb.write("runner-a", "k1", json!("v1")).await.unwrap();
        let mut snap = bb.snapshot().await;
        snap.events.push(snap.events[0].clone()); // seq 重复 → 不单调
        let err = match Blackboard::from_snapshot(snap, Policy::new(".")) {
            Err(e) => e,
            Ok(_) => panic!("应拒绝篡改的事件日志"),
        };
        assert!(err.contains("单调"), "{err}");
        let mut snap2 = bb.snapshot().await;
        snap2.seq = 99; // 与事件日志不一致
        let err2 = match Blackboard::from_snapshot(snap2, Policy::new(".")) {
            Err(e) => e,
            Ok(_) => panic!("应拒绝 seq 不一致的快照"),
        };
        assert!(err2.contains("不一致"), "{err2}");
    }

    #[tokio::test]
    async fn delete_removes_and_logs() {
        let bb = writable_blackboard();
        bb.write("runner-a", "k1", json!("v1")).await.unwrap();
        let removed = bb.delete("runner-a", "k1").await.unwrap();
        assert_eq!(removed.value, json!("v1"));
        assert!(matches!(
            bb.read("k1").await.unwrap_err(),
            BlackboardError::KeyNotFound(_)
        ));
        assert_eq!(bb.events().await.last().unwrap().op, BlackboardOp::Delete);
    }
}
