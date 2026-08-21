// R12:bus_store 完成，待主控接线
//! 总线持久化：关键消息落盘（追加式 JSONL 事件日志）+ 幂等去重 + 启动按序重放。
//!
//! 设计来源：《多Agent并行体系-生产级设计与跨机扩展-2026-08-16.md》§5 可靠性：
//! - **关键消息必须持久化**：Task/Result/Review/Refusal 全部落盘；进度类（Progress）按策略可选。
//! - **幂等去重**：按 [`crate::fleet::message_dedup_key`]（correlation_id + 种类 + 载荷摘要）
//!   以首次写入为准；重复提交/重放不影响结果。
//! - **崩溃恢复**：所有持久化消息以 JSON 行追加写入日志；启动时 [`BusStore::replay`]
//!   按写入顺序重放，损坏行跳过并计数，不阻塞恢复。
//! - **断点重放**：`AgentBus::replay_store` 把已持久化消息重新投递到已注册 agent，
//!   接收方以幂等去重（`dedupe_messages`）保证不重复执行。
//! - **远程 step 持久化**：[`persist_remote_event`] 把远程步骤生命周期事件
//!   （提交/审批/完成）转为总线消息落盘，崩溃后经 `replay` 按序重放。

use crate::fleet::{message_dedup_key, BusMessage, MessageKind, CONTROL_PLANE_AGENT};
use crate::remote_step::RemoteStepEvent;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 落盘策略：进度类是否落盘（关键消息不受此影响，始终落盘）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BusPersistPolicy {
    /// 进度类（Progress）是否落盘（默认 false）。
    pub persist_progress: bool,
}

/// 关键消息判定：除 Progress 外的消息（Task/Result/Review/Refusal）必须持久化。
pub fn is_critical(kind: MessageKind) -> bool {
    !matches!(kind, MessageKind::Progress)
}

/// 单条已持久化消息（日志行；dedup_key 为幂等索引键）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub dedup_key: String,
    pub msg: BusMessage,
    pub ts: String,
}

#[derive(Default, Debug)]
struct BusStoreInner {
    /// dedup_key → 消息（幂等索引）。
    committed: HashMap<String, BusMessage>,
    /// 写入顺序（重放保序）。
    order: Vec<String>,
    /// 日志重放损坏行计数。
    bad_lines: u64,
}

/// 总线持久化存储（Clone 共享同一索引与日志）。
#[derive(Clone, Default, Debug)]
pub struct BusStore {
    inner: Arc<Mutex<BusStoreInner>>,
    /// 追加式 JSONL 日志（None = 仅内存）。
    log_path: Option<PathBuf>,
    /// 落盘策略。
    policy: BusPersistPolicy,
}

impl BusStore {
    /// 新建存储；`log_path` 存在时自动重放（崩溃恢复）。
    pub fn new(log_path: Option<PathBuf>) -> Result<Self, String> {
        let store = Self {
            inner: Arc::new(Mutex::new(BusStoreInner::default())),
            log_path,
            policy: BusPersistPolicy::default(),
        };
        store.replay()?;
        Ok(store)
    }

    /// 纯内存存储（不落盘）。
    pub fn in_memory() -> Self {
        Self {
            inner: Arc::new(Mutex::new(BusStoreInner::default())),
            log_path: None,
            policy: BusPersistPolicy::default(),
        }
    }

    /// 设置落盘策略（进度类是否落盘）。
    pub fn with_policy(mut self, policy: BusPersistPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// 该消息种类是否应当持久化（关键消息恒真；进度类按策略）。
    pub fn should_persist(&self, kind: MessageKind) -> bool {
        is_critical(kind) || (matches!(kind, MessageKind::Progress) && self.policy.persist_progress)
    }

    /// 幂等写入：同 dedup_key 以首次为准（返回是否新写入）。持久化失败返回 Err。
    pub fn persist(&self, msg: &BusMessage) -> Result<bool, String> {
        let key = message_dedup_key(msg);
        let appended = {
            let mut inner = self
                .inner
                .lock()
                .map_err(|_| "总线存储锁异常".to_string())?;
            if inner.committed.contains_key(&key) {
                return Ok(false);
            }
            inner.order.push(key.clone());
            let _ = inner.committed.insert(key.clone(), msg.clone());
            StoredMessage {
                dedup_key: key.clone(),
                msg: msg.clone(),
                ts: chrono::Utc::now().to_rfc3339(),
            }
        };
        if let Some(path) = &self.log_path {
            self.append_log(path, &appended)?;
        }
        Ok(true)
    }

    /// 崩溃恢复：重放事件日志（幂等；损坏行跳过）。日志文件不存在视为空库。
    pub fn replay(&self) -> Result<(), String> {
        let Some(path) = &self.log_path else {
            return Ok(());
        };
        if !path.exists() {
            return Ok(());
        }
        let file = File::open(path).map_err(|e| format!("总线日志打开失败：{e}"))?;
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "总线存储锁异常".to_string())?;
        for line in BufReader::new(file).lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => {
                    inner.bad_lines += 1;
                    continue;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<StoredMessage>(&line) {
                Ok(stored) => {
                    if !inner.committed.contains_key(&stored.dedup_key) {
                        inner.order.push(stored.dedup_key.clone());
                        let _ = inner.committed.insert(stored.dedup_key, stored.msg);
                    }
                }
                Err(_) => inner.bad_lines += 1,
            }
        }
        Ok(())
    }

    /// 按写入顺序重放全部已持久化消息（启动断点恢复）。
    pub fn replay_messages(&self) -> Vec<BusMessage> {
        let inner = self.inner.lock().unwrap();
        inner
            .order
            .iter()
            .filter_map(|key| inner.committed.get(key).cloned())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|i| i.committed.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 日志重放损坏行计数（诊断用）。
    pub fn bad_log_lines(&self) -> u64 {
        self.inner.lock().map(|i| i.bad_lines).unwrap_or(0)
    }
}

impl BusStore {
    fn append_log(&self, path: &Path, stored: &StoredMessage) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("总线日志目录创建失败：{e}"))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("总线日志打开失败：{e}"))?;
        let mut line =
            serde_json::to_string(stored).map_err(|e| format!("总线消息序列化失败：{e}"))?;
        line.push('\n');
        file.write_all(line.as_bytes())
            .map_err(|e| format!("总线日志写入失败：{e}"))?;
        file.sync_all()
            .map_err(|e| format!("总线日志 fsync 失败：{e}"))
    }
}

/// 远程 step 事件持久化：把 [`RemoteStepEvent`] 转为总线消息（Task 种类）落盘，
/// 崩溃后经 [`BusStore::replay`]/[`BusStore::replay_messages`] 按序重放。
/// 幂等：同 correlation_id + 载荷只写一次。
pub fn persist_remote_event(store: &BusStore, event: &RemoteStepEvent) -> Result<bool, String> {
    let payload = serde_json::to_value(event).map_err(|e| format!("远程事件序列化失败：{e}"))?;
    let (correlation_id, to) = match event {
        RemoteStepEvent::Submitted {
            correlation_id,
            worker,
            ..
        } => (correlation_id.clone(), worker.clone()),
        RemoteStepEvent::ApprovalRequested { correlation_id, .. } => {
            (correlation_id.clone(), OWNER_DEVICE_AGENT.to_string())
        }
        RemoteStepEvent::ApprovalGranted { correlation_id, .. } => {
            (correlation_id.clone(), OWNER_DEVICE_AGENT.to_string())
        }
        RemoteStepEvent::Completed { correlation_id, .. } => {
            (correlation_id.clone(), CONTROL_PLANE_AGENT.to_string())
        }
    };
    let msg = BusMessage {
        id: 0,
        from: CONTROL_PLANE_AGENT.to_string(),
        to,
        kind: MessageKind::Task,
        correlation_id,
        payload,
    };
    store.persist(&msg)
}

/// 所有者设备 agent 标识（审批事件回传目标）。
pub const OWNER_DEVICE_AGENT: &str = "owner-device";

/// 节点状态事件持久化：节点注册/离线/恢复 → 总线消息落盘（崩溃重放不重复）。
/// 幂等键 = `node:status:<node_id>:<up|down>`（同状态只落一次；恢复/离线交替产生不同键）。
pub fn persist_node_status(
    store: &BusStore,
    node_id: &str,
    healthy: bool,
    detail: &str,
) -> Result<bool, String> {
    let msg = BusMessage {
        id: 0,
        from: CONTROL_PLANE_AGENT.to_string(),
        to: CONTROL_PLANE_AGENT.to_string(),
        kind: MessageKind::Task,
        correlation_id: format!(
            "node:status:{}:{}",
            node_id,
            if healthy { "up" } else { "down" }
        ),
        payload: serde_json::json!({
            "node_id": node_id,
            "healthy": healthy,
            "detail": detail,
        }),
    };
    store.persist(&msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::OverflowPolicy;

    fn msg(kind: MessageKind, correlation_id: &str, payload: serde_json::Value) -> BusMessage {
        BusMessage {
            id: 1,
            from: "a".to_string(),
            to: "b".to_string(),
            kind,
            correlation_id: correlation_id.to_string(),
            payload,
        }
    }

    #[test]
    fn critical_kinds_persist_by_default() {
        let store = BusStore::in_memory();
        assert!(store.should_persist(MessageKind::Task));
        assert!(store.should_persist(MessageKind::Result));
        assert!(store.should_persist(MessageKind::Review));
        assert!(store.should_persist(MessageKind::Refusal));
        assert!(
            !store.should_persist(MessageKind::Progress),
            "进度类默认不落盘"
        );
        let with_progress = BusStore::in_memory().with_policy(BusPersistPolicy {
            persist_progress: true,
        });
        assert!(with_progress.should_persist(MessageKind::Progress));
    }

    #[test]
    fn persist_is_idempotent_by_dedup_key() {
        let store = BusStore::in_memory();
        let m = msg(MessageKind::Task, "c1", serde_json::json!({"q": 1}));
        assert!(store.persist(&m).unwrap(), "首次写入");
        assert!(!store.persist(&m).unwrap(), "同 key 重复写入以首次为准");
        let m2 = msg(MessageKind::Task, "c1", serde_json::json!({"q": 2}));
        assert!(store.persist(&m2).unwrap(), "载荷不同 → 不同 key");
        assert_eq!(store.len(), 2);
        assert_eq!(store.replay_messages().len(), 2);
    }

    #[test]
    fn replay_recovers_messages_in_order() {
        let dir = std::env::temp_dir().join(format!("owo-bus-{}", std::process::id()));
        let path = dir.join("bus.jsonl");
        let _ = std::fs::remove_file(&path);
        {
            let store = BusStore::new(Some(path.clone())).unwrap();
            store
                .persist(&msg(MessageKind::Task, "c1", serde_json::json!({"n": 1})))
                .unwrap();
            store
                .persist(&msg(MessageKind::Result, "c1", serde_json::json!({"n": 2})))
                .unwrap();
        }
        let restored = BusStore::new(Some(path.clone())).unwrap();
        let msgs = restored.replay_messages();
        assert_eq!(msgs.len(), 2, "重放恢复全部消息");
        assert_eq!(msgs[0].correlation_id, "c1");
        assert_eq!(msgs[0].kind, MessageKind::Task);
        assert_eq!(msgs[1].kind, MessageKind::Result);
        // 幂等重放：再次 new 不改变索引。
        let again = BusStore::new(Some(path)).unwrap();
        assert_eq!(again.replay_messages().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_log_lines_are_skipped() {
        let dir = std::env::temp_dir().join(format!("owo-bus-bad-{}", std::process::id()));
        let path = dir.join("bus.jsonl");
        std::fs::create_dir_all(&dir).unwrap();
        let m = msg(MessageKind::Task, "c-ok", serde_json::json!({}));
        let line = format!(
            "not-json\n{}\n",
            serde_json::to_string(&StoredMessage {
                dedup_key: message_dedup_key(&m),
                msg: m,
                ts: "2026-01-01T00:00:00Z".to_string(),
            })
            .unwrap()
        );
        std::fs::write(&path, line).unwrap();
        let store = BusStore::new(Some(path.clone())).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(store.bad_log_lines(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overflow_policy_import_compiles() {
        // 保证 fleet 类型在测试中可引用（编译期冒烟）。
        let _ = OverflowPolicy::Reject;
    }
}
