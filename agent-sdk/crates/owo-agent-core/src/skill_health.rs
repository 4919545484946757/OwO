//! 技能健康度与自愈（v0.5 M-D，对应技术文档 5.8.3）。
//!
//! 连续 2 次失败标记 Degraded 并提示重新学习；窗口模板命中率下降触发重建，
//! 重建前坐标点击降级为询问；用户空闲时只读 OCR 校验模板提前预警。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// 技能状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SkillState {
    #[default]
    Active,
    /// 连续失败或模板命中率过低：执行前需确认/重新学习。
    Degraded,
    Disabled,
}

/// 一次失败的模式（步骤 + 原因 + 时间）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureMode {
    pub step: String,
    pub reason: String,
    pub at: String,
}

/// 单个技能的健康度指标。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillHealth {
    pub attempts: u64,
    pub successes: u64,
    pub consecutive_failures: u32,
    pub recent_failures: Vec<FailureMode>,
    pub state: SkillState,
    pub template_hits: u64,
    pub template_misses: u64,
}

impl Default for SkillHealth {
    fn default() -> Self {
        Self {
            attempts: 0,
            successes: 0,
            consecutive_failures: 0,
            recent_failures: Vec::new(),
            state: SkillState::Active,
            template_hits: 0,
            template_misses: 0,
        }
    }
}

impl SkillHealth {
    pub fn success_rate(&self) -> f64 {
        if self.attempts == 0 {
            0.0
        } else {
            self.successes as f64 / self.attempts as f64
        }
    }

    pub fn template_hit_rate(&self) -> f64 {
        let total = self.template_hits + self.template_misses;
        if total == 0 {
            0.0
        } else {
            self.template_hits as f64 / total as f64
        }
    }

    fn record_outcome(&mut self, ok: bool, failure: Option<FailureMode>) {
        self.attempts += 1;
        if ok {
            self.successes += 1;
            self.consecutive_failures = 0;
            self.state = SkillState::Active;
        } else {
            self.consecutive_failures += 1;
            if let Some(failure) = failure {
                self.recent_failures.push(failure);
                if self.recent_failures.len() > 16 {
                    self.recent_failures.remove(0);
                }
            }
            // 连续 2 次失败 → Degraded（设计文档阈值）。
            if self.consecutive_failures >= 2 && self.state == SkillState::Active {
                self.state = SkillState::Degraded;
            }
        }
    }
}

/// 健康度存储：按技能名维护，可持久化 JSON。
#[derive(Debug, Clone, Default)]
pub struct SkillHealthStore {
    entries: HashMap<String, SkillHealth>,
    path: Option<PathBuf>,
}

impl SkillHealthStore {
    pub fn new(path: Option<PathBuf>) -> Self {
        let mut store = Self {
            entries: HashMap::new(),
            path,
        };
        if let Some(path) = &store.path {
            store.entries = Self::load(path);
        }
        store
    }

    fn load(path: &PathBuf) -> HashMap<String, SkillHealth> {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    pub fn record(
        &mut self,
        skill_name: &str,
        ok: bool,
        failure: Option<FailureMode>,
    ) -> Result<SkillState, String> {
        let health = self.entries.entry(skill_name.to_string()).or_default();
        health.record_outcome(ok, failure);
        let state = health.state;
        self.save()?;
        Ok(state)
    }

    pub fn record_template_hit(&mut self, skill_name: &str, hit: bool) -> Result<(), String> {
        let health = self.entries.entry(skill_name.to_string()).or_default();
        if hit {
            health.template_hits += 1;
        } else {
            health.template_misses += 1;
        }
        // 模板命中率持续低于 0.4 → Degraded（配合 M-D 重建/询问）。
        if health.template_hits + health.template_misses >= 5
            && health.template_hit_rate() < 0.4
            && health.state == SkillState::Active
        {
            health.state = SkillState::Degraded;
        }
        self.save()
    }

    pub fn state(&self, skill_name: &str) -> SkillState {
        self.entries
            .get(skill_name)
            .map(|health| health.state)
            .unwrap_or(SkillState::Active)
    }

    pub fn health(&self, skill_name: &str) -> Option<&SkillHealth> {
        self.entries.get(skill_name)
    }

    pub fn list(&self) -> Vec<(String, SkillHealth)> {
        let mut items: Vec<(String, SkillHealth)> = self
            .entries
            .iter()
            .map(|(name, health)| (name.clone(), health.clone()))
            .collect();
        items.sort_by(|a, b| a.0.cmp(&b.0));
        items
    }

    pub fn reset(&mut self, skill_name: &str) -> Result<(), String> {
        self.entries
            .insert(skill_name.to_string(), SkillHealth::default());
        self.save()
    }

    fn save(&self) -> Result<(), String> {
        if let Some(path) = &self.path {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let json = serde_json::to_string_pretty(&self.entries).map_err(|e| e.to_string())?;
            std::fs::write(path, json).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_consecutive_failures_degrade_skill() {
        let mut store = SkillHealthStore::new(None);
        assert_eq!(
            store
                .record(
                    "send-file",
                    false,
                    Some(FailureMode {
                        step: "send".to_string(),
                        reason: "not found".to_string(),
                        at: chrono::Utc::now().to_rfc3339(),
                    })
                )
                .unwrap(),
            SkillState::Active
        );
        let state = store
            .record(
                "send-file",
                false,
                Some(FailureMode {
                    step: "send".to_string(),
                    reason: "not found".to_string(),
                    at: chrono::Utc::now().to_rfc3339(),
                }),
            )
            .unwrap();
        assert_eq!(state, SkillState::Degraded);
        assert_eq!(store.health("send-file").unwrap().success_rate(), 0.0);
    }

    #[test]
    fn success_recovers_skill() {
        let mut store = SkillHealthStore::new(None);
        for _ in 0..2 {
            store.record("skill", false, None).unwrap();
        }
        assert_eq!(store.state("skill"), SkillState::Degraded);
        let state = store.record("skill", true, None).unwrap();
        assert_eq!(state, SkillState::Active);
    }

    #[test]
    fn template_misses_degrade_after_threshold() {
        let mut store = SkillHealthStore::new(None);
        for _ in 0..6 {
            store.record_template_hit("qq-send", false).unwrap();
        }
        assert_eq!(store.state("qq-send"), SkillState::Degraded);
        let health = store.health("qq-send").unwrap();
        assert!(health.template_hit_rate() < 0.4);
    }
}
