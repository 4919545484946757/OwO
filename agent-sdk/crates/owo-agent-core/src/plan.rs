//! Plan：目标拆解为步骤依赖图（DAG），§12 Goal/Plan 编排层的数据模型。
//!
//! - 步骤含：前置依赖（DAG 边）、可并行标记、worker 规格（由 [`crate::goal::WorkerRegistry`] 派发）、
//!   验证断言（verify）、重试策略（预算内）。
//! - 非法环检测 + 缺失依赖校验；拓扑排序（wave 分层）供调度器使用。
//! - 序列化持久化（`<dir>/<plan_id>.json`），重启可恢复。

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// 步骤执行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepStatus {
    /// 已创建，等待依赖就绪。
    Pending,
    /// 依赖已全部成功，可被调度。
    Ready,
    /// 执行中。
    Running,
    /// 执行 + 验证通过。
    Succeeded,
    /// 重试耗尽或验证失败。
    Failed,
    /// abort/replan 时被中止。
    Aborted,
}

impl StepStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            StepStatus::Succeeded | StepStatus::Failed | StepStatus::Aborted
        )
    }

    pub fn can_resume(self) -> bool {
        matches!(
            self,
            StepStatus::Pending | StepStatus::Ready | StepStatus::Failed
        )
    }
}

/// 验证断言（步骤输出 / 目标验收条件共用）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationSpec {
    /// 输出包含子串。
    OutputContains(String),
    /// 输出与期望完全相等。
    OutputEquals(String),
    /// 输出非空。
    OutputNonEmpty,
    /// 保留扩展（自定义校验器名称，默认按“非空”处理）。
    Custom(String),
}

/// 对 worker 输出做验证断言（纯函数，可测）。
pub fn verify_output(spec: &VerificationSpec, output: &str) -> Result<(), String> {
    match spec {
        VerificationSpec::OutputContains(needle) => {
            if output.contains(needle.as_str()) {
                Ok(())
            } else {
                Err(format!(
                    "验证失败：输出缺少「{needle}」（实际：{}）",
                    preview(output)
                ))
            }
        }
        VerificationSpec::OutputEquals(expected) => {
            if output == expected.as_str() {
                Ok(())
            } else {
                Err(format!(
                    "验证失败：输出不等于「{expected}」（实际：{}）",
                    preview(output)
                ))
            }
        }
        VerificationSpec::OutputNonEmpty => {
            if output.trim().is_empty() {
                Err("验证失败：输出为空".to_string())
            } else {
                Ok(())
            }
        }
        VerificationSpec::Custom(_) => {
            if output.trim().is_empty() {
                Err("验证失败：自定义校验输出为空".to_string())
            } else {
                Ok(())
            }
        }
    }
}

fn preview(text: &str) -> String {
    let preview: String = text.chars().take(60).collect();
    if text.chars().count() > 60 {
        format!("{preview}…")
    } else {
        preview
    }
}

/// 计划步骤规格。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepSpec {
    /// 步骤唯一 id。
    pub id: String,
    /// 前置依赖步骤 id（DAG 边）。
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// 可并行标记：与依赖无冲突的步骤允许并行（调度器还受 max_parallel 限制）。
    #[serde(default)]
    pub parallel: bool,
    /// worker 名称（由 WorkerRegistry 按名派发）。
    pub worker: String,
    /// 传给 worker 的输入规格（任意 JSON）。
    #[serde(default)]
    pub input: serde_json::Value,
    /// 可选验证断言；缺省不验证（成功即通过）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify: Option<VerificationSpec>,
    /// 重试次数（预算内；失败/验证失败重试）。
    #[serde(default)]
    pub retries: u32,
}

impl StepSpec {
    pub fn new(id: impl Into<String>, worker: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            depends_on: Vec::new(),
            parallel: false,
            worker: worker.into(),
            input: serde_json::Value::Null,
            verify: None,
            retries: 0,
        }
    }
}

/// 计划：步骤 DAG + 元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    /// 所属目标 id。
    pub goal_id: String,
    /// 计划描述（人读）。
    pub description: String,
    pub steps: Vec<StepSpec>,
    pub created_at: String,
}

impl Plan {
    pub fn new(id: impl Into<String>, goal_id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            goal_id: goal_id.into(),
            description: String::new(),
            steps: Vec::new(),
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    pub fn add_step(&mut self, step: StepSpec) {
        self.steps.push(step);
    }

    pub fn step(&self, id: &str) -> Option<&StepSpec> {
        self.steps.iter().find(|s| s.id == id)
    }

    pub fn step_mut(&mut self, id: &str) -> Option<&mut StepSpec> {
        self.steps.iter_mut().find(|s| s.id == id)
    }

    /// 校验：步骤 id 唯一、依赖存在、无环（DFS 三色标记）。
    pub fn validate(&self) -> Result<(), String> {
        let mut seen = HashSet::new();
        for step in &self.steps {
            if !seen.insert(step.id.as_str()) {
                return Err(format!("步骤 id 重复：{}", step.id));
            }
            for dep in &step.depends_on {
                if dep == &step.id {
                    return Err(format!("步骤 {} 依赖自身", step.id));
                }
                if !self.step(dep).is_some() {
                    return Err(format!("步骤 {} 依赖不存在的步骤 {}", step.id, dep));
                }
            }
        }
        if let Some(cycle) = self.find_cycle() {
            return Err(format!("计划存在环：{}", cycle.join(" → ")));
        }
        Ok(())
    }

    /// 环检测（DFS 三色），返回构成环的步骤路径；无环返回 None。
    pub fn find_cycle(&self) -> Option<Vec<String>> {
        #[derive(Clone, Copy, PartialEq)]
        enum Color {
            White,
            Gray,
            Black,
        }
        let ids: Vec<String> = self.steps.iter().map(|s| s.id.clone()).collect();
        let mut colors: HashMap<&str, Color> =
            ids.iter().map(|id| (id.as_str(), Color::White)).collect();
        let mut stack: Vec<String> = Vec::new();

        fn dfs<'a>(
            plan: &'a Plan,
            id: &'a str,
            colors: &mut HashMap<&'a str, Color>,
            stack: &mut Vec<String>,
        ) -> Option<Vec<String>> {
            colors.insert(id, Color::Gray);
            stack.push(id.to_string());
            let step = plan.step(id)?;
            for dep in &step.depends_on {
                match colors.get(dep.as_str()) {
                    Some(Color::Gray) => {
                        // 找到环：从栈中 dep 位置截取。
                        let pos = stack.iter().position(|s| s == dep)?;
                        let mut cycle = stack[pos..].to_vec();
                        cycle.push(dep.clone());
                        return Some(cycle);
                    }
                    Some(Color::White) => {
                        if let Some(cycle) = dfs(plan, dep, colors, stack) {
                            return Some(cycle);
                        }
                    }
                    _ => {}
                }
            }
            stack.pop();
            colors.insert(id, Color::Black);
            None
        }

        for id in &ids {
            if colors.get(id.as_str()) == Some(&Color::White) {
                if let Some(cycle) = dfs(self, id, &mut colors, &mut stack) {
                    return Some(cycle);
                }
            }
        }
        None
    }

    /// 拓扑排序（wave 分层）：第 i 层 = 依赖都在 <i 层且不依赖同层/后层的步骤。
    /// 返回每层的步骤 id 列表（层内可并行；层间串行屏障）。
    pub fn topological_waves(&self) -> Result<Vec<Vec<String>>, String> {
        self.validate()?;
        let ids: Vec<&StepSpec> = self.steps.iter().collect();
        let mut wave_of: HashMap<&str, usize> = HashMap::new();
        // 迭代到不动点：wave = 1 + max(dep waves)；无依赖 = 1。
        loop {
            let mut changed = false;
            for step in &ids {
                let mut wave = 1usize;
                for dep in &step.depends_on {
                    if let Some(dep_wave) = wave_of.get(dep.as_str()) {
                        wave = wave.max(dep_wave + 1);
                    } else {
                        wave = usize::MAX; // 依赖尚未定层（有环时会卡住）
                    }
                }
                if wave != usize::MAX {
                    let current = wave_of.get(step.id.as_str()).copied();
                    if current != Some(wave) {
                        wave_of.insert(step.id.as_str(), wave);
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        // 校验全部定层（否则有环，但 validate 已拦）。
        if wave_of.len() != ids.len() {
            return Err("拓扑排序失败：存在环或缺失依赖".to_string());
        }
        let max_wave = wave_of.values().copied().max().unwrap_or(0);
        let mut waves = vec![Vec::new(); max_wave];
        for step in &ids {
            let wave = wave_of[step.id.as_str()];
            waves[wave - 1].push(step.id.clone());
        }
        Ok(waves)
    }

    /// 序列化持久化：`<dir>/<plan_id>.json`。
    pub fn persist(&self, dir: &Path) -> Result<PathBuf, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建计划目录失败：{e}"))?;
        let path = dir.join(format!("{}.json", self.id));
        let json =
            serde_json::to_string_pretty(self).map_err(|e| format!("计划序列化失败：{e}"))?;
        std::fs::write(&path, json).map_err(|e| format!("计划写入失败：{e}"))?;
        Ok(path)
    }

    /// 从磁盘加载计划。
    pub fn load(dir: &Path, plan_id: &str) -> Result<Plan, String> {
        let path = dir.join(format!("{plan_id}.json"));
        let json = std::fs::read_to_string(&path)
            .map_err(|e| format!("计划 {plan_id} 读取失败：{e}（{path:?}）"))?;
        serde_json::from_str(&json).map_err(|e| format!("计划 {plan_id} 解析失败：{e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_plan() -> Plan {
        let mut plan = Plan::new("p1", "g1");
        plan.add_step(StepSpec::new("a", "w1"));
        let mut b = StepSpec::new("b", "w2");
        b.depends_on = vec!["a".into()];
        b.verify = Some(VerificationSpec::OutputNonEmpty);
        plan.add_step(b);
        plan
    }

    #[test]
    fn plan_validate_ok() {
        assert!(sample_plan().validate().is_ok());
    }

    #[test]
    fn plan_validate_detects_cycle() {
        let mut plan = sample_plan();
        plan.add_step(StepSpec::new("c", "w3"));
        let mut d = StepSpec::new("d", "w4");
        d.depends_on = vec!["c".into()];
        plan.add_step(d);
        plan.step_mut("c").unwrap().depends_on = vec!["d".into()];
        let error = plan.validate().unwrap_err();
        assert!(error.contains("环"), "{error}");
        let cycle = plan.find_cycle().unwrap();
        assert!(cycle.len() >= 2, "{cycle:?}");
    }

    #[test]
    fn plan_validate_missing_dependency() {
        let mut plan = sample_plan();
        plan.step_mut("b").unwrap().depends_on = vec!["missing".into()];
        let error = plan.validate().unwrap_err();
        assert!(error.contains("不存在的步骤"));
    }

    #[test]
    fn plan_validate_duplicate_id() {
        let mut plan = sample_plan();
        plan.add_step(StepSpec::new("a", "w9"));
        let error = plan.validate().unwrap_err();
        assert!(error.contains("重复"));
    }

    #[test]
    fn topological_waves_three_parallel_join() {
        let mut plan = Plan::new("p2", "g1");
        for id in ["a", "b", "c"] {
            plan.add_step(StepSpec::new(id, "w1"));
        }
        let mut join = StepSpec::new("join", "w2");
        join.depends_on = vec!["a".into(), "b".into(), "c".into()];
        join.parallel = true;
        plan.add_step(join);
        let waves = plan.topological_waves().unwrap();
        assert_eq!(waves.len(), 2);
        assert_eq!(waves[0].len(), 3, "前 3 步应同层（可并行）：{waves:?}");
        assert_eq!(waves[1], vec!["join"]);
    }

    #[test]
    fn topological_waves_chain_is_serial() {
        let mut plan = Plan::new("p3", "g1");
        let mut prev: Option<String> = None;
        for i in 0..4 {
            let mut step = StepSpec::new(format!("s{i}"), "w1");
            if let Some(p) = &prev {
                step.depends_on = vec![p.clone()];
            }
            prev = Some(step.id.clone());
            plan.add_step(step);
        }
        let waves = plan.topological_waves().unwrap();
        assert_eq!(waves.len(), 4, "链式依赖应逐层：{waves:?}");
        for (i, wave) in waves.iter().enumerate() {
            assert_eq!(wave, &vec![format!("s{i}")]);
        }
    }

    #[test]
    fn plan_serde_roundtrip() {
        let plan = sample_plan();
        let json = serde_json::to_string(&plan).unwrap();
        let restored: Plan = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.id, plan.id);
        assert_eq!(restored.steps.len(), plan.steps.len());
        assert_eq!(
            restored.steps[1].verify,
            Some(VerificationSpec::OutputNonEmpty)
        );
    }

    #[test]
    fn plan_persist_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("owo-plan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plan = sample_plan();
        plan.persist(&dir).unwrap();
        let restored = Plan::load(&dir, "p1").unwrap();
        assert_eq!(restored.steps.len(), plan.steps.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_output_semantics() {
        assert!(verify_output(
            &VerificationSpec::OutputContains("ok".into()),
            "everything ok"
        )
        .is_ok());
        assert!(verify_output(&VerificationSpec::OutputContains("ok".into()), "nope").is_err());
        assert!(verify_output(&VerificationSpec::OutputEquals("x".into()), "x").is_ok());
        assert!(verify_output(&VerificationSpec::OutputEquals("x".into()), "y").is_err());
        assert!(verify_output(&VerificationSpec::OutputNonEmpty, "  ").is_err());
        assert!(verify_output(&VerificationSpec::OutputNonEmpty, "data").is_ok());
        assert!(verify_output(&VerificationSpec::Custom("x".into()), "data").is_ok());
    }
}
