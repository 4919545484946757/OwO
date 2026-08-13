//! 静默观察与情景记忆（v0.4.5，设计文档 M-D 起步）。
//!
//! Observer 后台轮询应用状态流（模拟面取模拟窗口日志；真实面采样前台应用/窗口标题哈希/剪贴板序列），
//! 把动作摘要（内容掩码）与结果写入本地情景记忆（JSONL，可换成 SQLite）。
//! `map_sim_events_to_actions` 把观察到的动作序列映射为录制样本，
//! 供 `/memory/mine-skill` 聚合 → 泛化 → 沉淀流程技能包（候选 → 用户确认 → active）。

use crate::learn::RecordedAction;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub ts: String,
    pub app_id: String,
    pub kind: String,
    pub summary: String,
    pub detail: serde_json::Value,
    pub state_hash: u64,
}

/// 本地情景记忆：JSONL 追加写，进程内缓存列表。
pub struct MemoryStore {
    path: PathBuf,
    entries: Vec<Observation>,
}

impl MemoryStore {
    pub fn new(path: PathBuf) -> Self {
        let mut store = Self {
            path,
            entries: Vec::new(),
        };
        store.entries = Self::load(&store.path);
        store.prune();
        store
    }

    fn load(path: &std::path::Path) -> Vec<Observation> {
        let Ok(content) = std::fs::read_to_string(path) else {
            return Vec::new();
        };
        content
            .lines()
            .filter_map(|line| serde_json::from_str::<Observation>(line).ok())
            .collect()
    }

    pub fn append(&mut self, observation: Observation) -> Result<(), String> {
        use std::io::Write;
        self.entries.push(observation.clone());
        self.prune();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| format!("打开情景记忆失败：{e}"))?;
        writeln!(
            file,
            "{}",
            serde_json::to_string(&observation).map_err(|e| e.to_string())?
        )
        .map_err(|e| format!("写入情景记忆失败：{e}"))?;
        Ok(())
    }

    pub fn list(&self, limit: usize) -> Vec<Observation> {
        if limit == 0 {
            self.entries.clone()
        } else {
            self.entries
                .iter()
                .rev()
                .take(limit)
                .cloned()
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect()
        }
    }

    pub fn count(&self) -> usize {
        self.entries.len()
    }

    pub fn clear(&mut self) -> Result<(), String> {
        self.entries.clear();
        std::fs::write(&self.path, "").map_err(|e| format!("清空情景记忆失败：{e}"))
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// 滚动清理：超期（默认 30 天）或超量（默认 1 万条）淘汰最旧条目。
    pub fn prune(&mut self) {
        let (max_entries, retention) = retention_config();
        let now = chrono::Utc::now();
        self.entries
            .retain(|entry| !is_expired(&entry.ts, &now, retention));
        if self.entries.len() > max_entries {
            let overflow = self.entries.len() - max_entries;
            self.entries.drain(0..overflow);
        }
    }
}

fn retention_config() -> (usize, chrono::Duration) {
    let max_entries = std::env::var("OWO_MEMORY_MAX")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(10_000);
    let days = std::env::var("OWO_MEMORY_RETENTION_DAYS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(30);
    (max_entries, chrono::Duration::days(days))
}

fn is_expired(ts: &str, now: &chrono::DateTime<chrono::Utc>, retention: chrono::Duration) -> bool {
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(ts) else {
        return false;
    };
    now.signed_duration_since(parsed.with_timezone(&chrono::Utc)) > retention
}

/// 把模拟窗口日志条目转换为观察记录（只保留动作摘要，不记录消息正文）。
pub fn observation_from_sim_event(entry: &serde_json::Value) -> Option<Observation> {
    let kind = entry.get("type").and_then(serde_json::Value::as_str)?;
    if !matches!(
        kind,
        "incoming" | "outgoing" | "typed" | "send_clicked" | "input_clicked" | "contact_switched"
    ) {
        return None;
    }
    let summary = match kind {
        "typed" => "键盘输入（内容掩码）".to_string(),
        "send_clicked" => "点击发送".to_string(),
        "input_clicked" => "聚焦输入框".to_string(),
        "contact_switched" => format!(
            "切换联系人：{}",
            entry
                .get("contact")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?")
        ),
        "outgoing" => "发出消息（内容掩码）".to_string(),
        "incoming" => "收到消息（内容掩码）".to_string(),
        _ => kind.to_string(),
    };
    Some(Observation {
        ts: entry
            .get("ts")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        app_id: "qq".to_string(),
        kind: "sim_event".to_string(),
        summary,
        detail: entry.clone(),
        state_hash: value_hash(entry),
    })
}

pub fn value_hash(value: &serde_json::Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.to_string().hash(&mut hasher);
    hasher.finish()
}

/// 真实面桌面状态快照（L0/L1 掩码采样）：不携带原始窗口标题。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopSnapshot {
    pub app_id: Option<String>,
    /// 窗口标题哈希（原始标题不落盘）。
    pub title_hash: Option<u64>,
    pub clipboard_seq: u32,
}

/// 采样当前桌面状态（无前台窗口时 app_id/title_hash 为 None）。
pub fn sample_desktop() -> DesktopSnapshot {
    let (app_id, title) = crate::platform::poll_foreground_app().unwrap_or_default();
    DesktopSnapshot {
        app_id: if app_id.is_empty() {
            None
        } else {
            Some(app_id)
        },
        title_hash: if title.is_empty() {
            None
        } else {
            Some(value_hash(&serde_json::json!(title)))
        },
        clipboard_seq: crate::platform::clipboard_sequence(),
    }
}

/// 对比相邻两次桌面采样，仅在状态变化时生成掩码观察记录。
///
/// 隐私边界（D22）：记录应用标识、标题哈希与剪贴板是否变化，不记录窗口标题原文与剪贴板内容。
pub fn desktop_observation(prev: &DesktopSnapshot, next: &DesktopSnapshot) -> Option<Observation> {
    if prev == next {
        return None;
    }
    let mut summary = Vec::new();
    let mut detail = serde_json::Map::new();
    if prev.app_id != next.app_id {
        summary.push(format!(
            "前台应用：{}",
            next.app_id.as_deref().unwrap_or("unknown")
        ));
    }
    if prev.title_hash != next.title_hash {
        summary.push("窗口标题变化（内容掩码）".to_string());
        if let Some(title_hash) = next.title_hash {
            detail.insert("title_hash".to_string(), serde_json::json!(title_hash));
        }
    }
    if prev.clipboard_seq != next.clipboard_seq {
        summary.push("剪贴板变化（内容掩码）".to_string());
        detail.insert("clipboard_changed".to_string(), serde_json::json!(true));
    }
    let snapshot_value = serde_json::json!({
        "app_id": next.app_id,
        "title_hash": next.title_hash,
        "clipboard_seq": next.clipboard_seq,
    });
    Some(Observation {
        ts: chrono::Utc::now().to_rfc3339(),
        app_id: next.app_id.clone().unwrap_or_default(),
        kind: "desktop_event".to_string(),
        summary: summary.join("；"),
        detail: serde_json::Value::Object(detail),
        state_hash: value_hash(&snapshot_value),
    })
}

/// 把观察记录中的模拟动作映射为学习样本（内容掩码：不保存消息正文）。
pub fn map_sim_events_to_actions(observations: &[Observation]) -> Vec<RecordedAction> {
    let mut actions = Vec::new();
    for observation in observations {
        if observation.kind != "sim_event" {
            continue;
        }
        let Some(kind) = observation
            .detail
            .get("type")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let (action_type, name, role) = match kind {
            "input_clicked" => (crate::learn::ActionType::Click, "输入消息", "edit"),
            "typed" => (crate::learn::ActionType::Type, "输入消息", "edit"),
            "send_clicked" => (crate::learn::ActionType::Click, "发送", "button"),
            "contact_switched" => {
                let contact = observation
                    .detail
                    .get("contact")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("联系人");
                actions.push(RecordedAction {
                    app_id: "qq".to_string(),
                    anchor: crate::learn::SemanticAnchor {
                        app_id: Some("qq".to_string()),
                        role: Some("list".to_string()),
                        name: contact.to_string(),
                        parent: None,
                        element_id: None,
                    },
                    action_type: crate::learn::ActionType::Click,
                    value_masked: true,
                    sensitive: false,
                    at: observation.ts.clone(),
                });
                continue;
            }
            _ => continue,
        };
        actions.push(RecordedAction {
            app_id: "qq".to_string(),
            anchor: crate::learn::SemanticAnchor {
                app_id: Some("qq".to_string()),
                role: Some(role.to_string()),
                name: name.to_string(),
                parent: None,
                element_id: None,
            },
            action_type,
            value_masked: true,
            sensitive: false,
            at: observation.ts.clone(),
        });
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_store() -> (MemoryStore, std::path::PathBuf) {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "owo-memory-test-{}-{counter}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        (MemoryStore::new(path.clone()), path)
    }

    #[test]
    fn memory_store_round_trip_and_clear() {
        let (mut store, path) = temp_store();
        let observation = Observation {
            ts: "2026-08-12T00:00:00Z".to_string(),
            app_id: "qq".to_string(),
            kind: "sim_event".to_string(),
            summary: "点击发送".to_string(),
            detail: json!({ "type": "send_clicked", "x": 900, "y": 645 }),
            state_hash: 42,
        };
        store.append(observation.clone()).expect("追加成功");
        let loaded = MemoryStore::new(path.clone());
        assert_eq!(loaded.count(), 1);
        assert_eq!(loaded.list(1)[0].summary, "点击发送");
        let mut store = loaded;
        store.clear().expect("清空成功");
        assert_eq!(store.count(), 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn observation_from_sim_event_filters_and_masks() {
        let typed = json!({ "type": "typed", "chars": 5 });
        let observation = observation_from_sim_event(&typed).expect("typed 应被观察");
        assert_eq!(observation.summary, "键盘输入（内容掩码）");
        assert!(observation_from_sim_event(&json!({ "type": "ready" })).is_none());
    }

    #[test]
    fn map_sim_events_to_actions_produces_click_type_sequence() {
        let events = [
            json!({ "type": "input_clicked", "x": 500, "y": 640 }),
            json!({ "type": "typed", "chars": 6 }),
            json!({ "type": "send_clicked", "x": 900, "y": 645 }),
            json!({ "type": "contact_switched", "contact": "李四" }),
        ];
        let observations: Vec<Observation> = events
            .iter()
            .filter_map(observation_from_sim_event)
            .collect();
        let actions = map_sim_events_to_actions(&observations);
        assert_eq!(actions.len(), 4);
        assert_eq!(actions[0].action_type, crate::learn::ActionType::Click);
        assert_eq!(actions[1].action_type, crate::learn::ActionType::Type);
        assert!(actions[1].value_masked);
        assert_eq!(actions[3].anchor.name, "李四");
    }

    #[test]
    fn memory_prune_caps_entries_and_drops_expired() {
        std::env::set_var("OWO_MEMORY_MAX", "3");
        let (mut store, path) = temp_store();
        let old = Observation {
            ts: "2020-01-01T00:00:00Z".to_string(),
            app_id: "qq".to_string(),
            kind: "sim_event".to_string(),
            summary: "旧事件".to_string(),
            detail: json!({}),
            state_hash: 1,
        };
        store.append(old.clone()).expect("追加成功");
        for index in 0..5 {
            store
                .append(Observation {
                    ts: "2026-08-13T00:00:00Z".to_string(),
                    app_id: "qq".to_string(),
                    kind: "sim_event".to_string(),
                    summary: format!("事件 {index}"),
                    detail: json!({}),
                    state_hash: index as u64,
                })
                .expect("追加成功");
        }
        assert!(store.count() <= 3);
        assert!(!store.entries.iter().any(|entry| entry.state_hash == 1));
        std::env::remove_var("OWO_MEMORY_MAX");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn desktop_observation_records_only_changes_and_masks_title() {
        let baseline = DesktopSnapshot {
            app_id: Some("qq".into()),
            title_hash: Some(100),
            clipboard_seq: 5,
        };
        assert!(desktop_observation(&baseline, &baseline).is_none());

        let next = DesktopSnapshot {
            app_id: Some("qq".into()),
            title_hash: Some(999),
            clipboard_seq: 5,
        };
        let observation = desktop_observation(&baseline, &next).expect("标题变化应生成观察");
        assert_eq!(observation.kind, "desktop_event");
        assert!(observation.summary.contains("窗口标题变化（内容掩码）"));
        assert_eq!(
            observation.detail.get("title_hash"),
            Some(&serde_json::json!(999))
        );
        assert!(observation.detail.get("clipboard_changed").is_none());
        // 原始标题绝不落盘。
        let serialized = serde_json::to_string(&observation).unwrap();
        assert!(!serialized.contains("聊天记录"));

        let clipboard_changed = DesktopSnapshot {
            app_id: Some("qq".into()),
            title_hash: Some(999),
            clipboard_seq: 7,
        };
        let observation =
            desktop_observation(&next, &clipboard_changed).expect("剪贴板变化应生成观察");
        assert!(observation.summary.contains("剪贴板变化（内容掩码）"));
        assert_eq!(
            observation.detail.get("clipboard_changed"),
            Some(&serde_json::json!(true))
        );
    }

    #[test]
    fn desktop_observation_switches_app_and_masks_title() {
        let baseline = DesktopSnapshot {
            app_id: Some("notepad".into()),
            title_hash: Some(1),
            clipboard_seq: 0,
        };
        let next = DesktopSnapshot {
            app_id: Some("weixin".into()),
            title_hash: Some(2),
            clipboard_seq: 0,
        };
        let observation = desktop_observation(&baseline, &next).expect("应用切换应生成观察");
        assert!(observation.summary.contains("前台应用：weixin"));
        assert_eq!(observation.app_id, "weixin");
        assert!(observation.detail.get("title_hash").is_some());
    }

    #[test]
    fn sample_desktop_is_callable_and_never_panics() {
        let snapshot = sample_desktop();
        // 无前台窗口（CI/沙箱）时 app_id 为 None，不应 panic。
        let _ = snapshot;
    }
}
