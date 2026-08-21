// R10:experience_store 完成（远程结果记录），待主控接线
//! Experience Store：worker/Goal 结果的幂等经验库 + 空闲期技能元数据聚合 + 崩溃恢复。
//!
//! 设计来源：《多Agent并行体系-生产级设计与跨机扩展-2026-08-16.md》§6 经验与持续学习：
//! - **幂等写入**：按 `correlation_id` 去重（首次写入生效，重放/重复提交不影响结果）。
//! - **崩溃恢复**：所有事件以 JSON 行追加写入事件日志（可选路径）；加载时重放日志，
//!   重建内存索引；日志损坏行跳过并计数，不阻塞恢复。
//! - **空闲期聚合**：[`ExperienceStore::aggregate`] 把成功/失败轨迹蒸馏为技能元数据更新
//!   （前置条件 = 成功轨迹观察到的输入键；断言 = 失败轨迹的错误模式；锚点先验 = 成功率）。
//!   [`ExperienceStore::run_aggregation`] 为可重放执行器：聚合结果落盘 JSON 报告，
//!   崩溃后 [`load_aggregation_report`] 读回，不重复计算。
//!   接入点：`goal.rs`（步骤/Goal 结果）、`fleet.rs`（fan-out 终态）。

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 事件种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperienceKind {
    /// 单次 worker 任务执行。
    WorkerTask,
    /// 整次 Goal 运行。
    GoalRun,
}

/// 结果归因（来源定位 + 观察元数据）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attribution {
    pub goal_id: Option<String>,
    pub plan_id: Option<String>,
    pub step_id: Option<String>,
    /// 输入 JSON 对象的键集合（前置条件候选；不含取值，避免敏感内容入经验）。
    pub input_keys: Vec<String>,
    /// 失败时的错误摘要（截断；成功为 None）。
    pub error: Option<String>,
}

/// 单条经验事件（事件日志的重放单元）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperienceEvent {
    pub correlation_id: String,
    pub worker: String,
    pub kind: ExperienceKind,
    pub outcome: Outcome,
    pub attribution: Attribution,
    pub ts: String,
}

/// 结果语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    Failure,
    Aborted,
    Cancelled,
}

impl Outcome {
    pub fn is_success(self) -> bool {
        self == Self::Success
    }
}

/// 聚合产出的技能元数据更新（蒸馏结果）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillInsight {
    pub worker: String,
    pub attempts: u64,
    pub successes: u64,
    pub success_rate: f64,
    /// 前置条件候选：成功轨迹中观察到的输入键。
    pub suggested_preconditions: Vec<String>,
    /// 断言候选：失败轨迹的错误模式（去重截断，最多 5 条）。
    pub suggested_assertions: Vec<String>,
    /// 锚点先验：该 worker 的历史成功率（0.0 ~ 1.0）。
    pub anchor_prior: f64,
}

/// 聚合执行器产物：技能元数据更新报告（可落盘、可重放）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AggregationReport {
    /// 生成时间（RFC3339）。
    pub generated_at: String,
    /// 参与聚合的事件总数（WorkerTask）。
    pub event_count: u64,
    /// 日志重放损坏行计数（诊断）。
    pub bad_log_lines: u64,
    /// 蒸馏出的技能元数据更新。
    pub insights: Vec<SkillInsight>,
}

impl AggregationReport {
    pub fn is_empty(&self) -> bool {
        self.insights.is_empty()
    }
}

/// 默认聚合报告文件名（`<report_dir>/skill_insights.json`）。
pub const AGGREGATION_REPORT_FILE: &str = "skill_insights.json";

const INSIGHT_ATTEMPTS_MIN: u64 = 2;
const ASSERTION_CAP: usize = 5;
const ASSERTION_MAX_LEN: usize = 80;

#[derive(Default, Debug)]
struct ExperienceInner {
    /// correlation_id → 事件（幂等索引）。
    events: HashMap<String, ExperienceEvent>,
    /// 写入顺序（事件视图保序）。
    order: Vec<String>,
    /// 日志重放损坏行计数。
    bad_lines: u64,
}

/// 经验库（Clone 共享同一索引与日志）。
#[derive(Clone, Default, Debug)]
pub struct ExperienceStore {
    inner: Arc<Mutex<ExperienceInner>>,
    /// 追加式 JSON 行日志（None = 仅内存）。
    log_path: Option<PathBuf>,
}

impl ExperienceStore {
    /// 新建经验库。`log_path` 存在时自动重放（崩溃恢复）；目录自动创建。
    pub fn new(log_path: Option<PathBuf>) -> Result<Self, String> {
        let store = Self {
            inner: Arc::new(Mutex::new(ExperienceInner::default())),
            log_path,
        };
        store.replay()?;
        Ok(store)
    }

    /// 纯内存经验库（不落盘）。
    pub fn in_memory() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ExperienceInner::default())),
            log_path: None,
        }
    }

    /// 崩溃恢复：重放事件日志（幂等；损坏行跳过）。日志文件不存在视为空库。
    pub fn replay(&self) -> Result<(), String> {
        let Some(path) = &self.log_path else {
            return Ok(());
        };
        if !path.exists() {
            return Ok(());
        }
        let file = File::open(path).map_err(|e| format!("经验日志打开失败：{e}"))?;
        let mut inner = self.inner.lock().map_err(|_| "经验库锁异常".to_string())?;
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
            match serde_json::from_str::<ExperienceEvent>(&line) {
                Ok(event) => {
                    inner.insert_event(event);
                }
                Err(_) => inner.bad_lines += 1,
            }
        }
        Ok(())
    }

    /// 幂等写入 worker 任务结果：同 correlation_id 重复写入以首次为准（返回 Ok）。
    pub fn record_worker_outcome(
        &self,
        correlation_id: impl Into<String>,
        worker: impl Into<String>,
        outcome: Outcome,
        attribution: Attribution,
    ) -> Result<(), String> {
        self.record(
            ExperienceEvent {
                correlation_id: correlation_id.into(),
                worker: worker.into(),
                kind: ExperienceKind::WorkerTask,
                outcome,
                attribution,
                ts: chrono::Utc::now().to_rfc3339(),
            },
            false,
        )
    }

    /// 幂等写入 Goal 运行结果。
    pub fn record_goal_outcome(
        &self,
        correlation_id: impl Into<String>,
        goal_id: impl Into<String>,
        outcome: Outcome,
        attribution: Attribution,
    ) -> Result<(), String> {
        self.record(
            ExperienceEvent {
                correlation_id: correlation_id.into(),
                worker: goal_id.into(),
                kind: ExperienceKind::GoalRun,
                outcome,
                attribution,
                ts: chrono::Utc::now().to_rfc3339(),
            },
            false,
        )
    }

    /// 记录远程步骤结果（worker = 目标节点；血缘并入 attribution.input_keys；
    /// 幂等键 = `remote:<step_id>`，崩溃重放不重复）。
    pub fn record_remote_outcome(
        &self,
        step_id: impl Into<String>,
        node: impl Into<String>,
        ok: bool,
        lineage: Vec<String>,
        error: Option<String>,
    ) -> Result<(), String> {
        let step_id = step_id.into();
        let outcome = if ok {
            Outcome::Success
        } else {
            Outcome::Failure
        };
        self.record_worker_outcome(
            format!("remote:{step_id}"),
            node,
            outcome,
            Attribution {
                goal_id: None,
                plan_id: None,
                step_id: Some(step_id),
                input_keys: lineage,
                error,
            },
        )
    }

    /// 底层幂等写入（内部与测试共用）。
    pub fn record(&self, event: ExperienceEvent, force: bool) -> Result<(), String> {
        let appended = {
            let mut inner = self.inner.lock().map_err(|_| "经验库锁异常".to_string())?;
            if !force && inner.events.contains_key(&event.correlation_id) {
                return Ok(());
            }
            inner.insert_event(event)
        };
        if let Some(path) = &self.log_path {
            self.append_log(path, &appended)?;
        }
        Ok(())
    }

    /// 事件视图（按写入顺序）。
    pub fn events(&self) -> Vec<ExperienceEvent> {
        let inner = self.inner.lock().unwrap();
        inner
            .order
            .iter()
            .filter_map(|id| inner.events.get(id).cloned())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|i| i.events.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 日志重放损坏行计数（诊断用）。
    pub fn bad_log_lines(&self) -> u64 {
        self.inner.lock().map(|i| i.bad_lines).unwrap_or(0)
    }

    /// 空闲期聚合：把成功/失败轨迹蒸馏为技能元数据更新。
    /// 仅聚合 WorkerTask 事件（GoalRun 不参与技能蒸馏）。
    pub fn aggregate(&self) -> Vec<SkillInsight> {
        let mut by_worker: HashMap<String, Vec<ExperienceEvent>> = HashMap::new();
        for event in self.events() {
            if event.kind == ExperienceKind::WorkerTask {
                by_worker
                    .entry(event.worker.clone())
                    .or_default()
                    .push(event);
            }
        }
        let mut insights: Vec<SkillInsight> = by_worker
            .into_iter()
            .map(|(worker, events)| distill(&worker, &events))
            .collect();
        insights.sort_by(|a, b| {
            b.attempts
                .cmp(&a.attempts)
                .then_with(|| a.worker.cmp(&b.worker))
        });
        insights
    }

    /// 聚合执行器（空闲期任务）：执行聚合并把报告落盘到 `<report_dir>/skill_insights.json`
    /// （整文件重写，天然幂等；崩溃后重跑结果一致）。
    pub fn run_aggregation(&self, report_dir: &Path) -> Result<AggregationReport, String> {
        let insights = self.aggregate();
        let event_count = insights.iter().map(|i| i.attempts).sum();
        let report = AggregationReport {
            generated_at: chrono::Utc::now().to_rfc3339(),
            event_count,
            bad_log_lines: self.bad_log_lines(),
            insights,
        };
        std::fs::create_dir_all(report_dir).map_err(|e| format!("聚合报告目录创建失败：{e}"))?;
        let path = report_dir.join(AGGREGATION_REPORT_FILE);
        let json = serde_json::to_string_pretty(&report)
            .map_err(|e| format!("聚合报告序列化失败：{e}"))?;
        std::fs::write(&path, json).map_err(|e| format!("聚合报告写入失败：{e}"))?;
        Ok(report)
    }
}

/// 崩溃重放：读取上次聚合报告（不存在返回 None）。
pub fn load_aggregation_report(report_dir: &Path) -> Option<AggregationReport> {
    let path = report_dir.join(AGGREGATION_REPORT_FILE);
    if !path.exists() {
        return None;
    }
    let json = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&json).ok()
}

impl ExperienceInner {
    fn insert_event(&mut self, event: ExperienceEvent) -> ExperienceEvent {
        if let Some(existing) = self.events.get(&event.correlation_id) {
            return existing.clone();
        }
        self.order.push(event.correlation_id.clone());
        let _ = self
            .events
            .insert(event.correlation_id.clone(), event.clone());
        event
    }
}

impl ExperienceStore {
    fn append_log(&self, path: &Path, event: &ExperienceEvent) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("经验日志目录创建失败：{e}"))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("经验日志打开失败：{e}"))?;
        let mut line =
            serde_json::to_string(event).map_err(|e| format!("经验事件序列化失败：{e}"))?;
        line.push('\n');
        file.write_all(line.as_bytes())
            .map_err(|e| format!("经验日志写入失败：{e}"))?;
        file.sync_all()
            .map_err(|e| format!("经验日志 fsync 失败：{e}"))
    }
}

/// 蒸馏单 worker 的成功/失败轨迹为技能元数据。
fn distill(worker: &str, events: &[ExperienceEvent]) -> SkillInsight {
    let attempts = events.len() as u64;
    let successes = events.iter().filter(|e| e.outcome.is_success()).count() as u64;
    let mut preconditions: Vec<String> = Vec::new();
    let mut assertions: VecDeque<String> = VecDeque::new();
    let mut seen_errors = HashMap::new();
    for event in events {
        if event.outcome.is_success() {
            for key in &event.attribution.input_keys {
                if !preconditions.contains(key) {
                    preconditions.push(key.clone());
                }
            }
        } else if let Some(err) = &event.attribution.error {
            let normalized = err.chars().take(ASSERTION_MAX_LEN).collect::<String>();
            if !seen_errors.contains_key(&normalized) {
                seen_errors.insert(normalized.clone(), ());
                if assertions.len() >= ASSERTION_CAP {
                    assertions.pop_front();
                }
                assertions.push_back(normalized);
            }
        }
    }
    preconditions.sort();
    let success_rate = if attempts >= INSIGHT_ATTEMPTS_MIN {
        successes as f64 / attempts as f64
    } else {
        0.0
    };
    SkillInsight {
        worker: worker.to_string(),
        attempts,
        successes,
        success_rate,
        suggested_preconditions: preconditions,
        suggested_assertions: assertions.into_iter().collect(),
        anchor_prior: success_rate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attribution(keys: &[&str], error: Option<&str>) -> Attribution {
        Attribution {
            goal_id: Some("g1".to_string()),
            plan_id: None,
            step_id: Some("s1".to_string()),
            input_keys: keys.iter().map(|k| k.to_string()).collect(),
            error: error.map(|e| e.to_string()),
        }
    }

    #[test]
    fn idempotent_write_by_correlation_id() {
        let store = ExperienceStore::in_memory();
        store
            .record_worker_outcome(
                "corr-1",
                "w1",
                Outcome::Success,
                attribution(&["text"], None),
            )
            .unwrap();
        store
            .record_worker_outcome(
                "corr-1",
                "w1",
                Outcome::Failure,
                attribution(&["text"], Some("boom")),
            )
            .unwrap();
        assert_eq!(store.len(), 1, "同 correlation_id 重复写入以首次为准");
        assert_eq!(store.events()[0].outcome, Outcome::Success);
    }

    #[test]
    fn aggregate_distills_success_and_failure_trajectories() {
        let store = ExperienceStore::in_memory();
        for i in 0..3 {
            store
                .record_worker_outcome(
                    format!("c{i}"),
                    "w1",
                    Outcome::Success,
                    attribution(&["text", "mode"], None),
                )
                .unwrap();
        }
        store
            .record_worker_outcome(
                "c-fail",
                "w1",
                Outcome::Failure,
                attribution(&["text"], Some("验证失败：输出不含关键字")),
            )
            .unwrap();
        let insights = store.aggregate();
        assert_eq!(insights.len(), 1);
        let insight = &insights[0];
        assert_eq!(insight.worker, "w1");
        assert_eq!(insight.attempts, 4);
        assert_eq!(insight.successes, 3);
        assert!((insight.success_rate - 0.75).abs() < 1e-9);
        assert!(insight
            .suggested_preconditions
            .contains(&"mode".to_string()));
        assert_eq!(insight.suggested_assertions.len(), 1);
        assert!(insight.suggested_assertions[0].contains("输出不含关键字"));
        assert!((insight.anchor_prior - 0.75).abs() < 1e-9);
    }

    #[test]
    fn log_replay_recovers_events_after_restart() {
        let dir = std::env::temp_dir().join(format!("owo-exp-{}", std::process::id()));
        let path = dir.join("experience.jsonl");
        let _ = std::fs::remove_file(&path);
        {
            let store = ExperienceStore::new(Some(path.clone())).unwrap();
            store
                .record_worker_outcome("corr-x", "w2", Outcome::Success, attribution(&["a"], None))
                .unwrap();
            store
                .record_worker_outcome(
                    "corr-y",
                    "w2",
                    Outcome::Failure,
                    attribution(&["b"], Some("err")),
                )
                .unwrap();
        }
        let restored = ExperienceStore::new(Some(path.clone())).unwrap();
        assert_eq!(restored.len(), 2, "重放恢复全部事件");
        assert_eq!(restored.events()[0].correlation_id, "corr-x");
        // 重放后再写新事件不重复。
        restored
            .record_worker_outcome("corr-x", "w2", Outcome::Success, attribution(&["a"], None))
            .unwrap();
        assert_eq!(restored.len(), 2);
        // 幂等重放：再次 new 不改变索引。
        let again = ExperienceStore::new(Some(path)).unwrap();
        assert_eq!(again.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_log_lines_are_skipped() {
        let dir = std::env::temp_dir().join(format!("owo-exp-bad-{}", std::process::id()));
        let path = dir.join("experience.jsonl");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            "not-json\n{\"correlation_id\":\"ok-1\",\"worker\":\"w\",\"kind\":\"worker_task\",\"outcome\":\"success\",\"attribution\":{\"goal_id\":null,\"plan_id\":null,\"step_id\":null,\"input_keys\":[],\"error\":null},\"ts\":\"2026-01-01T00:00:00Z\"}\n",
        )
        .unwrap();
        let store = ExperienceStore::new(Some(path.clone())).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(store.bad_log_lines(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
