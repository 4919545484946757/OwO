//! 自动化（v0.4 P1）：定时任务/提醒/监控，全部经审计；桌面端常驻时生效。
//!
//! v1 动作类型为提醒（Reminder）；定时触发后写入审计，桌面端轮询提醒列表。
//! 持久化：`<data>/automations.json`。

use chrono::{DateTime, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Schedule {
    OneShot { at: String },
    Interval { every_secs: u64 },
    Daily { time: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AutomationAction {
    Reminder { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationTask {
    pub id: String,
    pub name: String,
    pub schedule: Schedule,
    pub action: AutomationAction,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub last_run_at: Option<String>,
    #[serde(default)]
    pub created_at: String,
}

fn default_true() -> bool {
    true
}

impl AutomationTask {
    pub fn new(name: &str, schedule: Schedule, action: AutomationAction) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            schedule,
            action,
            enabled: true,
            last_run_at: None,
            created_at: Utc::now().to_rfc3339(),
        }
    }

    fn is_due(&self, now: DateTime<Utc>) -> bool {
        if !self.enabled {
            return false;
        }
        match &self.schedule {
            Schedule::OneShot { at } => {
                self.last_run_at.is_none()
                    && DateTime::parse_from_rfc3339(at)
                        .map(|due| now >= due.with_timezone(&Utc))
                        .unwrap_or(false)
            }
            Schedule::Interval { every_secs } => {
                let created = DateTime::parse_from_rfc3339(&self.created_at)
                    .map(|value| value.with_timezone(&Utc))
                    .unwrap_or(now);
                let last = self
                    .last_run_at
                    .as_deref()
                    .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                    .map(|value| value.with_timezone(&Utc));
                match last {
                    Some(last) => {
                        now.signed_duration_since(last).num_seconds() >= *every_secs as i64
                    }
                    None => now.signed_duration_since(created).num_seconds() >= *every_secs as i64,
                }
            }
            Schedule::Daily { time } => {
                let Ok(target) = NaiveTime::parse_from_str(time, "%H:%M") else {
                    return false;
                };
                let today = now.date_naive();
                let last_date = self
                    .last_run_at
                    .as_deref()
                    .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                    .map(|value| value.date_naive());
                last_date != Some(today) && now.time() >= target
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AutomationStore {
    root: PathBuf,
    tasks: HashMap<String, AutomationTask>,
    reminders: Vec<String>,
}

impl AutomationStore {
    pub fn new(root: PathBuf) -> Self {
        let mut store = Self {
            root,
            tasks: HashMap::new(),
            reminders: Vec::new(),
        };
        store.load();
        store
    }

    fn path(&self) -> PathBuf {
        self.root.join("automations.json")
    }

    fn load(&mut self) {
        let Ok(content) = std::fs::read_to_string(self.path()) else {
            return;
        };
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Ok(tasks) = serde_json::from_value::<Vec<AutomationTask>>(
                data.get("tasks").cloned().unwrap_or_default(),
            ) {
                for task in tasks {
                    self.tasks.insert(task.id.clone(), task);
                }
            }
            if let Ok(reminders) = serde_json::from_value::<Vec<String>>(
                data.get("reminders").cloned().unwrap_or_default(),
            ) {
                self.reminders = reminders;
            }
        }
    }

    pub fn save(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.root).map_err(|error| error.to_string())?;
        let mut tasks: Vec<AutomationTask> = self.tasks.values().cloned().collect();
        tasks.sort_by(|left, right| left.created_at.cmp(&right.created_at));
        let data = serde_json::json!({
            "tasks": tasks,
            "reminders": self.reminders,
        });
        std::fs::write(
            self.path(),
            serde_json::to_string_pretty(&data).map_err(|e| e.to_string())?,
        )
        .map_err(|error| error.to_string())
    }

    pub fn list(&self) -> Vec<AutomationTask> {
        let mut tasks: Vec<AutomationTask> = self.tasks.values().cloned().collect();
        tasks.sort_by(|left, right| left.created_at.cmp(&right.created_at));
        tasks
    }

    pub fn get(&self, id: &str) -> Option<&AutomationTask> {
        self.tasks.get(id)
    }

    pub fn upsert(&mut self, task: AutomationTask) -> Result<(), String> {
        self.tasks.insert(task.id.clone(), task);
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> Result<(), String> {
        self.tasks.remove(id);
        self.save()
    }

    pub fn toggle(&mut self, id: &str) -> Result<bool, String> {
        let enabled = {
            let task = self
                .tasks
                .get_mut(id)
                .ok_or_else(|| format!("任务不存在：{id}"))?;
            task.enabled = !task.enabled;
            task.enabled
        };
        self.save()?;
        Ok(enabled)
    }

    pub fn due_tasks(&self, now: DateTime<Utc>) -> Vec<String> {
        self.tasks
            .values()
            .filter(|task| task.is_due(now))
            .map(|task| task.id.clone())
            .collect()
    }

    /// 触发任务：标记 last_run_at，提醒动作追加到提醒列表；返回动作文本。
    pub fn fire(&mut self, id: &str, now: DateTime<Utc>) -> Result<String, String> {
        let task = self
            .tasks
            .get_mut(id)
            .ok_or_else(|| format!("任务不存在：{id}"))?;
        task.last_run_at = Some(now.to_rfc3339());
        let text = match &task.action {
            AutomationAction::Reminder { text } => text.clone(),
        };
        self.reminders.push(text.clone());
        if self.reminders.len() > 200 {
            self.reminders.drain(..self.reminders.len() - 200);
        }
        self.save()?;
        Ok(text)
    }

    pub fn reminders(&self) -> &[String] {
        &self.reminders
    }

    pub fn clear_reminders(&mut self) -> Result<(), String> {
        self.reminders.clear();
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn root() -> PathBuf {
        std::env::temp_dir().join(format!("owo-automation-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn interval_fires_after_elapsed() {
        let now = Utc.with_ymd_and_hms(2026, 8, 12, 12, 0, 0).unwrap();
        let mut task = AutomationTask::new(
            "check",
            Schedule::Interval { every_secs: 60 },
            AutomationAction::Reminder {
                text: "检查".to_string(),
            },
        );
        task.created_at = now.to_rfc3339();
        assert!(!task.is_due(now));
        let later = now + chrono::Duration::seconds(61);
        assert!(task.is_due(later));
        task.last_run_at = Some(later.to_rfc3339());
        assert!(!task.is_due(later));
    }

    #[test]
    fn one_shot_fires_once() {
        let now = Utc.with_ymd_and_hms(2026, 8, 12, 12, 0, 0).unwrap();
        let at = (now + chrono::Duration::seconds(10)).to_rfc3339();
        let task = AutomationTask::new(
            "once",
            Schedule::OneShot { at },
            AutomationAction::Reminder {
                text: "单次".to_string(),
            },
        );
        assert!(!task.is_due(now));
        let later = now + chrono::Duration::seconds(11);
        assert!(task.is_due(later));
        let mut fired = task.clone();
        fired.last_run_at = Some(later.to_rfc3339());
        assert!(!fired.is_due(later + chrono::Duration::seconds(60)));
    }

    #[test]
    fn daily_fires_once_per_day() {
        let now = Utc.with_ymd_and_hms(2026, 8, 12, 9, 0, 0).unwrap();
        let task = AutomationTask::new(
            "daily",
            Schedule::Daily {
                time: "08:00".to_string(),
            },
            AutomationAction::Reminder {
                text: "日报".to_string(),
            },
        );
        assert!(task.is_due(now));
        let mut fired = task.clone();
        fired.last_run_at = Some(now.to_rfc3339());
        assert!(!fired.is_due(now + chrono::Duration::hours(1)));
        let next_day = now + chrono::Duration::days(1);
        assert!(fired.is_due(next_day));
    }

    #[test]
    fn store_persists_and_fires() {
        let dir = root();
        let mut store = AutomationStore::new(dir.clone());
        let now = Utc::now();
        let mut task = AutomationTask::new(
            "remind-me",
            Schedule::Interval { every_secs: 1 },
            AutomationAction::Reminder {
                text: "休息一下".to_string(),
            },
        );
        task.created_at = (now - chrono::Duration::seconds(2)).to_rfc3339();
        store.upsert(task.clone()).unwrap();
        let due = store.due_tasks(now);
        assert_eq!(due.len(), 1);
        let text = store.fire(&due[0], now).unwrap();
        assert_eq!(text, "休息一下");
        assert_eq!(store.reminders().len(), 1);
        assert!(store.due_tasks(now).is_empty());

        let mut reloaded = AutomationStore::new(dir.clone());
        assert_eq!(reloaded.list().len(), 1);
        assert_eq!(reloaded.reminders().len(), 1);
        reloaded.clear_reminders().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn toggle_disables_task() {
        let dir = root();
        let mut store = AutomationStore::new(dir.clone());
        let now = Utc::now();
        let mut task = AutomationTask::new(
            "t",
            Schedule::Interval { every_secs: 1 },
            AutomationAction::Reminder {
                text: "x".to_string(),
            },
        );
        task.created_at = (now - chrono::Duration::seconds(5)).to_rfc3339();
        store.upsert(task).unwrap();
        assert_eq!(store.due_tasks(now).len(), 1);
        store.toggle(&store.list()[0].id).unwrap();
        assert!(store.due_tasks(now).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
