//! 操作学习（v0.4 D23/D24/D26）：示范学习 + 受限自主探索双轨、
//! 动作图（Action Graph）、流程技能包、主动建议。
//!
//! 安全边界：
//! - 敏感面（密码/支付/验证码）在任何轨都熔断：不学习、不记录。
//! - 学习样本默认掩码，消息内容不采样；录制可暂停、可一键清空。
//! - 主动建议默认仅提示，不执行。

use crate::settings::ProactiveSettings;
use crate::skill_health::{FailureMode, SkillHealth, SkillHealthStore, SkillState};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

// ---------- 动作图 ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionType {
    Click,
    Type,
    Shortcut,
    Inject,
    /// 启动应用或打开 URL（主动打开，不依赖应用已在前台）。
    Launch,
    /// 按屏幕坐标点击（OCR 定位的自绘控件，如 QQ 红包/表情面板）。
    ClickAt,
    /// 滚动（v0.5 M-B 动作程序扩展）。
    Scroll,
    /// 拖拽（v0.5 M-B 动作程序扩展）。
    Drag,
    /// 等待（v0.5 M-B 动作程序扩展）。
    Wait,
    /// 结构化断言动作（v0.5 M-B，配合 assert.rs 的 VerificationRecipe）。
    Assert,
    /// 悬停（v0.5 M-B 动作程序扩展）。
    Hover,
    /// 右键点击（v0.5 M-B 动作程序扩展）。
    RightClick,
    /// 双击（v0.5 M-B 动作程序扩展）。
    DoubleClick,
}

/// 语义锚点：以无障碍角色 + 名称定位，坐标只作辅助，不作为主定位依据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticAnchor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub name: String,
    /// 稳定元素 ID（窗口元素注册表）：存在时优先按注册表坐标定位，
    /// 避免每次动作都重新做 UIA/OCR 定位。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element_id: Option<String>,
    /// 父容器名称（如“会话列表”），用于消除同名节点歧义。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionNode {
    pub id: String,
    pub action_type: ActionType,
    pub anchor: SemanticAnchor,
    /// 变量模板：`{contact}` 之类，执行时由用户或情景模型填充。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_template: Option<String>,
    /// 操作后状态验证描述（窗口/标题/内容是否变化）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionEdge {
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precondition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionGraph {
    pub version: u32,
    pub start: String,
    pub nodes: Vec<ActionNode>,
    pub edges: Vec<ActionEdge>,
}

impl Default for ActionGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionGraph {
    pub fn new() -> Self {
        Self {
            version: 1,
            start: String::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    pub fn add_node(
        &mut self,
        id: impl Into<String>,
        action_type: ActionType,
        anchor: SemanticAnchor,
        value_template: Option<String>,
        verify: Option<String>,
    ) {
        let id = id.into();
        if self.start.is_empty() {
            self.start = id.clone();
        }
        self.nodes.push(ActionNode {
            id,
            action_type,
            anchor,
            value_template,
            verify,
        });
    }

    pub fn add_edge(
        &mut self,
        from: impl Into<String>,
        to: impl Into<String>,
        precondition: Option<String>,
        verify: Option<String>,
    ) {
        self.edges.push(ActionEdge {
            from: from.into(),
            to: to.into(),
            precondition,
            verify,
        });
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.nodes.is_empty() {
            return Err("动作图没有节点".to_string());
        }
        if !self.nodes.iter().any(|node| node.id == self.start) {
            return Err(format!("起始节点不存在：{}", self.start));
        }
        let mut ids = std::collections::HashSet::new();
        for node in &self.nodes {
            if !ids.insert(node.id.as_str()) {
                return Err(format!("重复节点 id：{}", node.id));
            }
        }
        for edge in &self.edges {
            if !ids.contains(edge.from.as_str()) || !ids.contains(edge.to.as_str()) {
                return Err(format!("边引用不存在的节点：{} -> {}", edge.from, edge.to));
            }
        }
        Ok(())
    }

    /// 提取 `{var}` 变量名。
    pub fn variables(&self) -> Vec<String> {
        let mut vars = Vec::new();
        for node in &self.nodes {
            if let Some(template) = &node.value_template {
                for token in template.split('{').skip(1) {
                    if let Some(end) = token.find('}') {
                        let name = token[..end].trim().to_string();
                        if !name.is_empty() && !vars.contains(&name) {
                            vars.push(name);
                        }
                    }
                }
            }
        }
        vars
    }
}

// ---------- 流程技能包 ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    None,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowSkillManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub min_app_version: String,
    /// 目标应用白名单（app_id）。
    pub target_apps: Vec<String>,
    pub permissions: Vec<String>,
    pub variables: Vec<String>,
    /// 敏感面声明（必填；None 视为未声明，校验不通过）。
    pub sensitivity: Sensitivity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowSkillPackage {
    pub manifest: FlowSkillManifest,
    pub graph: ActionGraph,
    pub skill_md: String,
}

impl FlowSkillPackage {
    pub fn validate(&self) -> Result<(), String> {
        if self.manifest.id.is_empty() || self.manifest.name.is_empty() {
            return Err("manifest 缺少 id/name".to_string());
        }
        if self.manifest.target_apps.is_empty() {
            return Err("manifest.target_apps 不能为空".to_string());
        }
        if self.manifest.sensitivity == Sensitivity::None {
            return Err("manifest.sensitivity 必填".to_string());
        }
        self.graph.validate()?;
        for variable in self.graph.variables() {
            if !self.manifest.variables.contains(&variable) {
                return Err(format!("动作图变量未在 manifest 声明：{variable}"));
            }
        }
        if !self.skill_md.trim_start().starts_with("---") {
            return Err("SKILL.md 缺少 frontmatter".to_string());
        }
        Ok(())
    }
}

/// 流程技能包存储：`<data>/skills/user/<name>/`（SKILL.md + graph.json + manifest.json）。
pub struct FlowSkillStore {
    root: PathBuf,
    health: std::sync::Mutex<SkillHealthStore>,
}

impl FlowSkillStore {
    pub fn new(root: PathBuf) -> Self {
        let health = SkillHealthStore::new(Some(root.join("health.json")));
        Self {
            root,
            health: std::sync::Mutex::new(health),
        }
    }

    fn package_dir(&self, name: &str) -> Result<PathBuf, String> {
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(format!("非法技能名：{name}"));
        }
        Ok(self.root.join(name))
    }

    pub fn save(&self, package: &FlowSkillPackage) -> Result<PathBuf, String> {
        package.validate()?;
        let dir = self.package_dir(&package.manifest.name)?;
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        std::fs::write(dir.join("SKILL.md"), &package.skill_md)
            .map_err(|error| error.to_string())?;
        std::fs::write(
            dir.join("graph.json"),
            serde_json::to_string_pretty(&package.graph).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_string_pretty(&package.manifest).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        Ok(dir)
    }

    pub fn list(&self) -> Result<Vec<String>, String> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Ok(Vec::new());
        };
        let mut names = Vec::new();
        for entry in entries.flatten() {
            let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
            if is_dir && entry.path().join("SKILL.md").exists() {
                if let Some(name) = entry.file_name().to_str() {
                    names.push(name.to_string());
                }
            }
        }
        names.sort();
        Ok(names)
    }

    pub fn load(&self, name: &str) -> Result<FlowSkillPackage, String> {
        let dir = self.package_dir(name)?;
        let skill_md =
            std::fs::read_to_string(dir.join("SKILL.md")).map_err(|error| error.to_string())?;
        let graph = serde_json::from_str(
            &std::fs::read_to_string(dir.join("graph.json")).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let manifest = serde_json::from_str(
            &std::fs::read_to_string(dir.join("manifest.json"))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let package = FlowSkillPackage {
            manifest,
            graph,
            skill_md,
        };
        package.validate()?;
        Ok(package)
    }

    pub fn delete(&self, name: &str) -> Result<(), String> {
        let dir = self.package_dir(name)?;
        if !dir.exists() {
            return Err(format!("技能包不存在：{name}"));
        }
        std::fs::remove_dir_all(&dir).map_err(|error| error.to_string())
    }

    /// 记录一次执行结果（成功/失败），失败带步骤与原因；返回最新状态。
    pub fn record_execution(
        &self,
        name: &str,
        ok: bool,
        step: &str,
        reason: &str,
    ) -> Result<SkillState, String> {
        self.load(name)?;
        let failure = if ok {
            None
        } else {
            Some(FailureMode {
                step: step.to_string(),
                reason: reason.to_string(),
                at: Utc::now().to_rfc3339(),
            })
        };
        self.health
            .lock()
            .map_err(|_| "健康度存储锁中毒".to_string())?
            .record(name, ok, failure)
    }

    /// 执行门禁：Disabled 一律拒绝；Degraded 需显式 degraded_ack。
    pub fn execution_gate(&self, name: &str, degraded_ack: bool) -> Result<(), String> {
        self.load(name)?;
        let state = self
            .health
            .lock()
            .map_err(|_| "健康度存储锁中毒".to_string())?
            .state(name);
        match state {
            SkillState::Disabled => Err(format!("技能已禁用：{name}")),
            SkillState::Degraded if !degraded_ack => Err(format!(
                "技能处于降级状态（连续失败或模板命中率过低），需 degraded_ack:true 确认后执行：{name}"
            )),
            _ => Ok(()),
        }
    }

    pub fn health_state(&self, name: &str) -> SkillState {
        self.health
            .lock()
            .map(|health| health.state(name))
            .unwrap_or(SkillState::Active)
    }

    pub fn list_health(&self) -> Vec<(String, SkillHealth)> {
        self.health
            .lock()
            .map(|health| health.list())
            .unwrap_or_default()
    }

    pub fn reset_health(&self, name: &str) -> Result<(), String> {
        self.load(name)?;
        self.health
            .lock()
            .map_err(|_| "健康度存储锁中毒".to_string())?
            .reset(name)
    }
}

// ---------- 示范学习录制 ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearnState {
    Idle,
    Recording,
    Paused,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedAction {
    pub app_id: String,
    pub anchor: SemanticAnchor,
    pub action_type: ActionType,
    /// 输入内容默认掩码：不保存真实消息/密码。
    #[serde(default)]
    pub value_masked: bool,
    /// 调用方标记的敏感面（密码框/支付/验证码），一旦触发立即熔断。
    #[serde(default)]
    pub sensitive: bool,
    pub at: String,
}

fn keyword_breaks(anchor: &SemanticAnchor) -> bool {
    ["password", "支付", "密码", "验证码", "captcha", "card"]
        .iter()
        .any(|keyword| {
            anchor.name.contains(keyword)
                || anchor
                    .role
                    .as_deref()
                    .map(|role| role.contains(keyword))
                    .unwrap_or(false)
        })
}

pub struct LearnRecorder {
    state: LearnState,
    actions: Vec<RecordedAction>,
    sensitive_break: bool,
}

impl Default for LearnRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl LearnRecorder {
    pub fn new() -> Self {
        Self {
            state: LearnState::Idle,
            actions: Vec::new(),
            sensitive_break: false,
        }
    }

    pub fn start(&mut self) {
        self.state = LearnState::Recording;
    }

    pub fn pause(&mut self) {
        if self.state == LearnState::Recording {
            self.state = LearnState::Paused;
        }
    }

    pub fn resume(&mut self) {
        if self.state == LearnState::Paused {
            self.state = LearnState::Recording;
        }
    }

    /// 结束录制并返回本次样本。
    pub fn stop(&mut self) -> Vec<RecordedAction> {
        self.state = LearnState::Idle;
        self.sensitive_break = false;
        std::mem::take(&mut self.actions)
    }

    pub fn clear(&mut self) {
        self.actions.clear();
        self.sensitive_break = false;
    }

    pub fn record(&mut self, action: RecordedAction) -> Result<(), String> {
        if self.state != LearnState::Recording {
            return Err("未在录制（/learn start 开始）".to_string());
        }
        if action.sensitive || keyword_breaks(&action.anchor) {
            self.sensitive_break = true;
            return Err("敏感面熔断：密码/支付/验证码等场景不学习、不记录".to_string());
        }
        self.actions.push(action);
        Ok(())
    }

    pub fn state(&self) -> LearnState {
        self.state
    }

    pub fn actions(&self) -> &[RecordedAction] {
        &self.actions
    }

    pub fn samples(&self) -> usize {
        self.actions.len()
    }

    pub fn sensitive_break(&self) -> bool {
        self.sensitive_break
    }
}

/// 录制样本 → 动作图（泛化）：同一锚点的 Type 动作出现 ≥2 次，
/// 推断为变量 `{value}`（消息内容不采样，只按锚点重复度推断）。
pub fn generalize_to_graph(samples: &[RecordedAction]) -> Result<ActionGraph, String> {
    if samples.is_empty() {
        return Err("没有录制样本".to_string());
    }
    let mut graph = ActionGraph::new();
    for (index, action) in samples.iter().enumerate() {
        if action.sensitive || keyword_breaks(&action.anchor) {
            return Err("样本含敏感面，已拒绝沉淀".to_string());
        }
        let id = format!("step-{}", index + 1);
        let value_template = if action.action_type == ActionType::Type {
            let repeated = samples
                .iter()
                .filter(|other| {
                    other.action_type == ActionType::Type
                        && other.anchor.name == action.anchor.name
                        && other.anchor.role == action.anchor.role
                })
                .count();
            if repeated >= 2 {
                Some("{value}".to_string())
            } else {
                None
            }
        } else {
            None
        };
        graph.add_node(
            id,
            action.action_type,
            action.anchor.clone(),
            value_template,
            None,
        );
    }
    for index in 0..samples.len().saturating_sub(1) {
        graph.add_edge(
            format!("step-{}", index + 1),
            format!("step-{}", index + 2),
            None,
            None,
        );
    }
    graph.validate()?;
    Ok(graph)
}

/// 多轨迹对齐 + 变量推断（v0.5 M-C，对应技术文档 5.8.3）。
///
/// 归一化（动作类型 + 语义锚点）→ 多轨迹编辑距离对齐（以最长轨迹为骨干）→
/// 变量边界推断（骨干位置稳定但 Type 动作跨轨迹重复 → `{value}`）→ 线性动作图。
///
/// 输入内容默认掩码（不保存真实消息），因此变量推断基于“位置稳定 + 锚点重复”，
/// 与 `generalize_to_graph` 的单轨迹启发式一致，但增加了跨轨迹一致性约束：
/// 只有 ≥2 条轨迹在相同位置出现相同锚点的 Type 动作才推断为变量。
pub fn generalize_traces(traces: &[Vec<RecordedAction>]) -> Result<ActionGraph, String> {
    if traces.len() < 2 {
        // 少于 2 条轨迹时回退到单轨迹泛化（保持既有行为兼容）。
        if let Some(single) = traces.first() {
            return generalize_to_graph(single);
        }
        return Err("没有可泛化的轨迹".to_string());
    }

    for trace in traces {
        if trace.is_empty() {
            return Err("存在空轨迹".to_string());
        }
        for action in trace {
            if action.sensitive || keyword_breaks(&action.anchor) {
                return Err("轨迹含敏感面，已拒绝泛化".to_string());
            }
        }
    }

    // 骨干 = 最长轨迹；其余轨迹与骨干做编辑距离对齐。
    let backbone_idx = traces
        .iter()
        .enumerate()
        .max_by_key(|(_, trace)| trace.len())
        .map(|(index, _)| index)
        .unwrap_or(0);
    let backbone = &traces[backbone_idx];
    let backbone_keys: Vec<String> = backbone.iter().map(normalized_action_key).collect();

    // aligned[i] = 第 i 条轨迹中与骨干第 j 个位置对齐的索引（Option）。
    let mut aligned: Vec<Vec<Option<usize>>> = Vec::new();
    for (index, trace) in traces.iter().enumerate() {
        if index == backbone_idx {
            aligned.push((0..trace.len()).map(Some).collect());
            continue;
        }
        let other_keys: Vec<String> = trace.iter().map(normalized_action_key).collect();
        aligned.push(align_to_backbone(&backbone_keys, &other_keys));
    }

    let mut graph = ActionGraph::new();
    for (position, action) in backbone.iter().enumerate() {
        if action.sensitive || keyword_breaks(&action.anchor) {
            return Err("样本含敏感面，已拒绝沉淀".to_string());
        }
        let id = format!("step-{}", position + 1);

        // 跨轨迹一致性：有多少条轨迹在该位置出现相同归一化锚点。
        let consistent = aligned
            .iter()
            .enumerate()
            .filter(|(trace_idx, mapping)| {
                if *trace_idx == backbone_idx {
                    return true;
                }
                mapping[position]
                    .map(|other_idx| {
                        normalized_action_key(&traces[*trace_idx][other_idx])
                            == backbone_keys[position]
                    })
                    .unwrap_or(false)
            })
            .count();

        let value_template = if action.action_type == ActionType::Type && consistent >= 2 {
            Some("{value}".to_string())
        } else {
            None
        };
        graph.add_node(
            id,
            action.action_type,
            action.anchor.clone(),
            value_template,
            None,
        );
    }
    for index in 0..backbone.len().saturating_sub(1) {
        graph.add_edge(
            format!("step-{}", index + 1),
            format!("step-{}", index + 2),
            None,
            None,
        );
    }
    graph.validate()?;
    Ok(graph)
}

/// 动作归一化键：动作类型 + 角色 + 名称（不含坐标与内容）。
fn normalized_action_key(action: &RecordedAction) -> String {
    format!(
        "{:?}/{}//{}",
        action.action_type,
        action.anchor.role.as_deref().unwrap_or(""),
        action.anchor.name
    )
}

/// 编辑距离对齐：返回骨干每个位置在 other 中的最佳对齐索引（gap 为 None）。
///
/// 使用 Needleman-Wunsch（匹配 0 / 错配 1 / 空位 1）并回溯。
fn align_to_backbone(backbone: &[String], other: &[String]) -> Vec<Option<usize>> {
    let m = backbone.len();
    let n = other.len();
    // score[i][j]：骨干前 i 个与 other 前 j 个的最小编辑代价。
    let mut score = vec![vec![0usize; n + 1]; m + 1];
    score[0] = (0..=n).collect();
    for (i, row) in score.iter_mut().enumerate().skip(1) {
        row[0] = i;
    }
    for i in 1..=m {
        for j in 1..=n {
            let cost = if backbone[i - 1] == other[j - 1] {
                0
            } else {
                1
            };
            score[i][j] = (score[i - 1][j - 1] + cost)
                .min(score[i - 1][j] + 1)
                .min(score[i][j - 1] + 1);
        }
    }
    let mut result = vec![None; m];
    let (mut i, mut j) = (m, n);
    while i > 0 {
        if j > 0
            && score[i][j] == score[i - 1][j - 1] + usize::from(backbone[i - 1] != other[j - 1])
        {
            result[i - 1] = Some(j - 1);
            i -= 1;
            j -= 1;
        } else if i > 0 && score[i][j] == score[i - 1][j] + 1 {
            i -= 1; // 骨干位置在 other 中缺失（gap）。
        } else if j > 0 && score[i][j] == score[i][j - 1] + 1 {
            j -= 1; // other 中的额外动作。
        } else {
            // 防御：理论不可达，按匹配处理。
            if j > 0 {
                result[i - 1] = Some(j - 1);
                i -= 1;
                j -= 1;
            } else {
                i -= 1;
            }
        }
    }
    result
}

/// 沉淀门槛（v0.5 M-C）：同 app + 归一化序列 ≥3 次且成功率 ≥80% 才生成候选。
///
/// `outcomes` 与 `traces` 等长时为各轨迹的成功判定；为空时按未知处理（只查数量门槛）。
pub fn candidate_eligible(traces: &[Vec<RecordedAction>], outcomes: &[bool]) -> Result<(), String> {
    if traces.len() < 3 {
        return Err(format!(
            "沉淀门槛未达：需要 ≥3 条轨迹，当前 {} 条",
            traces.len()
        ));
    }
    let first_app = traces[0]
        .first()
        .map(|action| action.app_id.as_str())
        .unwrap_or("");
    if first_app.is_empty() {
        return Err("沉淀门槛未达：轨迹缺少 app_id".to_string());
    }
    if traces
        .iter()
        .any(|trace| trace.iter().any(|action| action.app_id != first_app))
    {
        return Err("沉淀门槛未达：轨迹必须属于同一应用".to_string());
    }
    if !outcomes.is_empty() && outcomes.len() != traces.len() {
        return Err("沉淀门槛未达：outcomes 与轨迹数量不一致".to_string());
    }
    if !outcomes.is_empty() {
        let successes = outcomes.iter().filter(|ok| **ok).count();
        let rate = successes as f64 / outcomes.len() as f64;
        if rate < 0.8 {
            return Err(format!(
                "沉淀门槛未达：成功率 {:.1}% < 80%（{} 次成功 / {} 次尝试）",
                rate * 100.0,
                successes,
                outcomes.len()
            ));
        }
    }
    Ok(())
}

/// 示范学习流水线：录制 → 泛化 → 沉淀流程技能包。
pub struct LearnPipeline {
    pub recorder: LearnRecorder,
    pub store: FlowSkillStore,
    last_samples: Vec<RecordedAction>,
}

impl LearnPipeline {
    pub fn new(store_root: PathBuf) -> Self {
        Self {
            recorder: LearnRecorder::new(),
            store: FlowSkillStore::new(store_root),
            last_samples: Vec::new(),
        }
    }

    /// 结束录制并保留样本（供随后沉淀）。
    pub fn stop_recording(&mut self) -> Vec<RecordedAction> {
        let samples = self.recorder.stop();
        self.last_samples = samples.clone();
        samples
    }

    /// 结束录制并沉淀为流程技能包（SKILL.md + graph.json + manifest.json）。
    pub fn sink_skill(
        &mut self,
        name: &str,
        target_apps: Vec<String>,
        sensitivity: Sensitivity,
        description: &str,
    ) -> Result<FlowSkillPackage, String> {
        let samples = if self.last_samples.is_empty() {
            self.recorder.stop()
        } else {
            std::mem::take(&mut self.last_samples)
        };
        self.sink_from_actions(name, target_apps, sensitivity, description, samples)
    }

    /// 直接由动作样本沉淀流程技能包（主动建议“学习”确认、静默观察挖掘共用）。
    pub fn sink_from_actions(
        &self,
        name: &str,
        target_apps: Vec<String>,
        sensitivity: Sensitivity,
        description: &str,
        samples: Vec<RecordedAction>,
    ) -> Result<FlowSkillPackage, String> {
        let graph = generalize_to_graph(&samples)?;
        let variables = graph.variables();
        let package = FlowSkillPackage {
            manifest: FlowSkillManifest {
                id: format!("com.owo.learned.{name}"),
                name: name.to_string(),
                version: "1.0.0".to_string(),
                min_app_version: "0.4.0".to_string(),
                target_apps,
                permissions: vec!["ui:operate".to_string(), "text:inject".to_string()],
                variables,
                sensitivity,
            },
            graph,
            skill_md: format!(
                "---\nname: {name}\ndescription: {description}\n---\n由示范学习自动生成，可编辑。"
            ),
        };
        package.validate()?;
        self.store.save(&package)?;
        Ok(package)
    }
}

/// 把主动建议的动作序列字符串解析为学习样本。
///
/// 支持 `动作:锚点名`（click/type），无前缀时按 click 处理；
/// Type 样本一律 `value_masked=true`（不采样消息正文）。
/// 其他动作前缀暂不映射（动作图泛化仅对 Click/Type 有稳定语义）。
pub fn recorded_actions_from_sequence(app_id: &str, sequence: &[String]) -> Vec<RecordedAction> {
    let now = Utc::now().to_rfc3339();
    sequence
        .iter()
        .filter_map(|item| {
            let item = item.trim();
            if item.is_empty() {
                return None;
            }
            let (prefix, anchor_name) = match item.split_once(':') {
                Some((prefix, name)) => (prefix.trim().to_lowercase(), name.trim().to_string()),
                None => ("click".to_string(), item.to_string()),
            };
            if anchor_name.is_empty() {
                return None;
            }
            let action_type = if prefix == "type" {
                ActionType::Type
            } else {
                ActionType::Click
            };
            let role = match (action_type, anchor_name.as_str()) {
                (ActionType::Type, _) => Some("edit".to_string()),
                (ActionType::Click, name) if name.contains("发送") || name.contains("搜索") => {
                    Some("button".to_string())
                }
                (ActionType::Click, name) if name.contains("输入") => Some("edit".to_string()),
                _ => None,
            };
            Some(RecordedAction {
                app_id: app_id.to_string(),
                anchor: SemanticAnchor {
                    app_id: Some(app_id.to_string()),
                    role,
                    name: anchor_name,
                    parent: None,
                    element_id: None,
                },
                action_type,
                value_masked: action_type == ActionType::Type,
                sensitive: false,
                at: now.clone(),
            })
        })
        .collect()
}

// ---------- 主动建议 ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProactiveSuggestion {
    pub id: String,
    pub app_id: String,
    pub summary: String,
    pub sequence: Vec<String>,
    pub created_at: String,
    /// 默认仅提示；自动执行需单独开启。
    #[serde(default)]
    pub auto_exec: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionAction {
    Learn,
    #[serde(alias = "execute")]
    ExecuteOnce,
    Ignore,
    #[serde(alias = "mute")]
    MuteForever,
}

struct SequenceRecord {
    app_id: String,
    actions: Vec<String>,
    day: String,
}

pub struct ProactiveEngine {
    settings: ProactiveSettings,
    history: VecDeque<SequenceRecord>,
    suggestions: Vec<ProactiveSuggestion>,
    ignored: HashMap<String, u32>,
    muted_until: HashMap<String, String>,
    last_shown: HashMap<String, DateTime<Utc>>,
    shown_today: u32,
    current_day: String,
    suppressed: bool,
}

impl ProactiveEngine {
    pub fn new(settings: ProactiveSettings) -> Self {
        let now = Utc::now();
        Self {
            settings,
            history: VecDeque::new(),
            suggestions: Vec::new(),
            ignored: HashMap::new(),
            muted_until: HashMap::new(),
            last_shown: HashMap::new(),
            shown_today: 0,
            current_day: now.date_naive().to_string(),
            suppressed: false,
        }
    }

    pub fn set_suppressed(&mut self, suppressed: bool) {
        self.suppressed = suppressed;
    }

    /// 运行时应用主动建议设置（设置页即时生效）。
    pub fn apply_settings(&mut self, settings: ProactiveSettings) {
        self.settings = settings;
    }

    fn sequence_key(app_id: &str, actions: &[String]) -> String {
        format!("{app_id}:{}", actions.join("|"))
    }

    fn similar(left: &[String], right: &[String], threshold: f64) -> bool {
        if left == right {
            return true;
        }
        let set_left: std::collections::HashSet<&str> = left.iter().map(String::as_str).collect();
        let set_right: std::collections::HashSet<&str> = right.iter().map(String::as_str).collect();
        let overlap = set_left.intersection(&set_right).count();
        let max = set_left.len().max(set_right.len()).max(1);
        (overlap as f64 / max as f64) >= threshold
    }

    /// 观察一次动作序列；达到阈值时返回建议（仅提示，不执行）。
    pub fn observe(&mut self, app_id: &str, actions: Vec<String>) -> Option<ProactiveSuggestion> {
        let now = Utc::now();
        let day = now.date_naive().to_string();
        if self.current_day != day {
            self.current_day = day.clone();
            self.shown_today = 0;
        }
        let cutoff = (now - ChronoDuration::days(7)).date_naive().to_string();
        self.history
            .retain(|record| record.day.as_str() >= cutoff.as_str());
        self.history.push_back(SequenceRecord {
            app_id: app_id.to_string(),
            actions: actions.clone(),
            day: day.clone(),
        });

        if !self.settings.enabled || self.suppressed || actions.is_empty() {
            return None;
        }
        let key = Self::sequence_key(app_id, &actions);
        if self
            .muted_until
            .get(&key)
            .map(|until| until.as_str() >= day.as_str())
            .unwrap_or(false)
        {
            return None;
        }
        if let Some(last) = self.last_shown.get(&key) {
            if now - *last < ChronoDuration::hours(self.settings.cooldown_hours as i64) {
                return None;
            }
        }
        if self.shown_today >= self.settings.daily_cap {
            return None;
        }

        let same_today = self
            .history
            .iter()
            .filter(|record| {
                record.app_id == app_id
                    && record.day == day
                    && Self::similar(&record.actions, &actions, self.settings.similarity)
            })
            .count();
        let same_week = self
            .history
            .iter()
            .filter(|record| {
                record.app_id == app_id
                    && Self::similar(&record.actions, &actions, self.settings.similarity)
            })
            .count();
        let hit = same_today >= self.settings.daily_threshold as usize
            || same_week >= self.settings.weekly_threshold as usize;
        if !hit {
            return None;
        }

        let suggestion = ProactiveSuggestion {
            id: uuid::Uuid::new_v4().to_string(),
            app_id: app_id.to_string(),
            summary: format!(
                "检测到重复操作（近 7 天 {} 次，今天 {} 次），是否沉淀为技能或下次帮你完成？",
                same_week, same_today
            ),
            sequence: actions.clone(),
            created_at: now.to_rfc3339(),
            auto_exec: false,
        };
        self.last_shown.insert(key.clone(), now);
        self.shown_today += 1;
        self.suggestions.push(suggestion.clone());
        Some(suggestion)
    }

    /// 用户对建议做出选择：忽略 2 次自动静默 30 天；永久静默/学习/执行一次。
    pub fn decide(&mut self, suggestion_id: &str, action: SuggestionAction) -> Result<(), String> {
        let suggestion = self
            .suggestions
            .iter()
            .find(|suggestion| suggestion.id == suggestion_id)
            .cloned()
            .ok_or_else(|| format!("建议不存在：{suggestion_id}"))?;
        let key = Self::sequence_key(&suggestion.app_id, &suggestion.sequence);
        let now = Utc::now();
        match action {
            SuggestionAction::Ignore => {
                let count = self.ignored.entry(key.clone()).or_default();
                *count += 1;
                if *count >= 2 {
                    self.muted_until.insert(
                        key,
                        (now + ChronoDuration::days(self.settings.auto_silence_days as i64))
                            .date_naive()
                            .to_string(),
                    );
                }
            }
            SuggestionAction::MuteForever => {
                self.muted_until.insert(key, "9999-12-31".to_string());
            }
            SuggestionAction::Learn | SuggestionAction::ExecuteOnce => {
                // 学习交给 LearnRecorder/流程技能包流程；执行仍需审批。
                self.last_shown.remove(&key);
                if action == SuggestionAction::Learn {
                    // 已确认沉淀：从建议列表移除，避免重复学习。
                    self.suggestions
                        .retain(|suggestion| suggestion.id != suggestion_id);
                }
            }
        }
        Ok(())
    }

    pub fn suggestions(&self) -> &[ProactiveSuggestion] {
        &self.suggestions
    }

    pub fn history_len(&self) -> usize {
        self.history.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph_with_contact() -> ActionGraph {
        let mut graph = ActionGraph::new();
        graph.add_node(
            "find",
            ActionType::Click,
            SemanticAnchor {
                app_id: Some("qq".to_string()),
                role: Some("search_box".to_string()),
                name: "联系人搜索框".to_string(),
                parent: None,
                element_id: None,
            },
            None,
            Some("窗口标题变化".to_string()),
        );
        graph.add_node(
            "type",
            ActionType::Type,
            SemanticAnchor {
                app_id: Some("qq".to_string()),
                role: Some("input".to_string()),
                name: "搜索输入".to_string(),
                parent: None,
                element_id: None,
            },
            Some("{contact}".to_string()),
            None,
        );
        graph.add_edge("find", "type", None, None);
        graph
    }

    #[test]
    fn action_graph_validation_and_variables() {
        let graph = graph_with_contact();
        assert!(graph.validate().is_ok());
        assert_eq!(graph.variables(), vec!["contact"]);
        let mut broken = graph.clone();
        broken.edges.push(ActionEdge {
            from: "type".to_string(),
            to: "missing".to_string(),
            precondition: None,
            verify: None,
        });
        assert!(broken.validate().is_err());
    }

    #[test]
    fn semantic_anchor_element_id_round_trip_and_backward_compat() {
        let anchor = SemanticAnchor {
            app_id: Some("qq".into()),
            role: Some("button".into()),
            name: "发送".into(),
            element_id: Some("qq:button:3".into()),
            parent: Some("会话列表".into()),
        };
        let json = serde_json::to_string(&anchor).expect("序列化");
        assert!(json.contains("qq:button:3"));
        let restored: SemanticAnchor = serde_json::from_str(&json).expect("反序列化");
        assert_eq!(restored.element_id.as_deref(), Some("qq:button:3"));

        // 旧格式（无 element_id 字段）必须仍可解析，且默认 None。
        let legacy = r#"{"app_id":"qq","role":"button","name":"发送","parent":"会话列表"}"#;
        let legacy_anchor: SemanticAnchor = serde_json::from_str(legacy).expect("旧格式兼容");
        assert!(legacy_anchor.element_id.is_none());
        assert_eq!(legacy_anchor.name, "发送");
    }

    #[test]
    fn flow_skill_package_requires_sensitivity_and_declared_variables() {
        let graph = graph_with_contact();
        let package = FlowSkillPackage {
            manifest: FlowSkillManifest {
                id: "com.example.send-file".to_string(),
                name: "send-file".to_string(),
                version: "1.0.0".to_string(),
                min_app_version: "0.4.0".to_string(),
                target_apps: vec!["qq".to_string()],
                permissions: vec!["text:inject".to_string()],
                variables: vec!["contact".to_string()],
                sensitivity: Sensitivity::Low,
            },
            graph,
            skill_md: "---\nname: send-file\ndescription: 发送文件\n---\n流程".to_string(),
        };
        assert!(package.validate().is_ok());

        let mut no_sensitivity = package.clone();
        no_sensitivity.manifest.sensitivity = Sensitivity::None;
        assert!(no_sensitivity.validate().is_err());

        let mut undeclared = package.clone();
        undeclared.manifest.variables = Vec::new();
        assert!(undeclared.validate().is_err());
    }

    #[test]
    fn flow_skill_store_round_trip_list_and_delete() {
        let root = std::env::temp_dir().join(format!("owo-learn-store-{}", uuid::Uuid::new_v4()));
        let store = FlowSkillStore::new(root.join("skills").join("user"));
        let package = FlowSkillPackage {
            manifest: FlowSkillManifest {
                id: "com.example.demo".to_string(),
                name: "demo-flow".to_string(),
                version: "1.0.0".to_string(),
                min_app_version: "0.4.0".to_string(),
                target_apps: vec!["qq".to_string()],
                permissions: vec!["text:inject".to_string()],
                variables: vec!["contact".to_string()],
                sensitivity: Sensitivity::Medium,
            },
            graph: graph_with_contact(),
            skill_md: "---\nname: demo-flow\n---\nbody".to_string(),
        };
        let dir = store.save(&package).unwrap();
        assert!(dir.join("SKILL.md").exists());
        assert!(dir.join("graph.json").exists());
        assert!(dir.join("manifest.json").exists());
        assert_eq!(store.list().unwrap(), vec!["demo-flow"]);
        let loaded = store.load("demo-flow").unwrap();
        assert_eq!(loaded.manifest.name, "demo-flow");
        assert_eq!(loaded.graph.nodes.len(), 2);
        store.delete("demo-flow").unwrap();
        assert!(store.list().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn recorder_requires_start_and_breaks_on_sensitive() {
        let mut recorder = LearnRecorder::new();
        assert!(recorder
            .record(RecordedAction {
                app_id: "qq".to_string(),
                anchor: SemanticAnchor {
                    app_id: Some("qq".to_string()),
                    role: None,
                    name: "发送按钮".to_string(),
                    parent: None,
                    element_id: None,
                },
                action_type: ActionType::Click,
                value_masked: true,
                sensitive: false,
                at: "now".to_string(),
            })
            .is_err());
        recorder.start();
        recorder
            .record(RecordedAction {
                app_id: "qq".to_string(),
                anchor: SemanticAnchor {
                    app_id: Some("qq".to_string()),
                    role: None,
                    name: "发送按钮".to_string(),
                    parent: None,
                    element_id: None,
                },
                action_type: ActionType::Click,
                value_masked: true,
                sensitive: false,
                at: "now".to_string(),
            })
            .unwrap();
        assert_eq!(recorder.samples(), 1);
        assert!(recorder
            .record(RecordedAction {
                app_id: "qq".to_string(),
                anchor: SemanticAnchor {
                    app_id: Some("qq".to_string()),
                    role: None,
                    name: "密码输入框".to_string(),
                    parent: None,
                    element_id: None,
                },
                action_type: ActionType::Type,
                value_masked: true,
                sensitive: true,
                at: "now".to_string(),
            })
            .is_err());
        assert!(recorder.sensitive_break());
        recorder.pause();
        assert_eq!(recorder.state(), LearnState::Paused);
        recorder.resume();
        let samples = recorder.stop();
        assert_eq!(samples.len(), 1);
        assert_eq!(recorder.state(), LearnState::Idle);
    }

    #[test]
    fn proactive_engine_thresholds_cooldown_and_ignore_silence() {
        let settings = ProactiveSettings {
            enabled: true,
            weekly_threshold: 5,
            daily_threshold: 3,
            similarity: 0.9,
            cooldown_hours: 24,
            daily_cap: 3,
            auto_silence_days: 30,
        };
        let mut engine = ProactiveEngine::new(settings);
        let actions = vec![
            "focus".to_string(),
            "select_conversation".to_string(),
            "click_send".to_string(),
        ];
        assert!(engine.observe("qq", actions.clone()).is_none());
        assert!(engine.observe("qq", actions.clone()).is_none());
        let suggestion = engine
            .observe("qq", actions.clone())
            .expect("daily threshold");
        assert!(!suggestion.auto_exec);
        assert!(engine.observe("qq", actions.clone()).is_none()); // cooldown
        engine
            .decide(&suggestion.id, SuggestionAction::Ignore)
            .unwrap();
        engine
            .decide(&suggestion.id, SuggestionAction::Ignore)
            .unwrap();
        assert!(!engine.muted_until.is_empty()); // 忽略 2 次后进入静默
        let key = "qq:focus|select_conversation|click_send";
        assert!(engine.muted_until.contains_key(key));
    }

    #[test]
    fn apply_settings_updates_runtime_thresholds() {
        let mut engine = ProactiveEngine::new(ProactiveSettings {
            enabled: false,
            weekly_threshold: 5,
            daily_threshold: 5,
            similarity: 1.0,
            cooldown_hours: 0,
            daily_cap: 10,
            auto_silence_days: 30,
        });
        let actions = vec!["a".to_string(), "b".to_string()];
        assert!(engine.observe("app", actions.clone()).is_none());
        engine.apply_settings(ProactiveSettings {
            enabled: true,
            weekly_threshold: 3,
            daily_threshold: 1,
            similarity: 1.0,
            cooldown_hours: 0,
            daily_cap: 10,
            auto_silence_days: 30,
        });
        let suggestion = engine.observe("app", actions.clone()).unwrap();
        assert!(!suggestion.auto_exec);
    }

    #[test]
    fn proactive_engine_can_be_suppressed_and_muted_forever() {
        let settings = ProactiveSettings {
            enabled: true,
            weekly_threshold: 3,
            daily_threshold: 3,
            similarity: 1.0,
            cooldown_hours: 0,
            daily_cap: 10,
            auto_silence_days: 30,
        };
        let mut engine = ProactiveEngine::new(settings);
        engine.set_suppressed(true);
        let actions = vec!["a".to_string(), "b".to_string()];
        assert!(engine.observe("app", actions.clone()).is_none());
        engine.set_suppressed(false);
        assert!(engine.observe("app", actions.clone()).is_none());
        let suggestion = engine.observe("app", actions.clone()).unwrap();
        engine
            .decide(&suggestion.id, SuggestionAction::MuteForever)
            .unwrap();
        assert_eq!(
            engine.muted_until.get("app:a|b").map(String::as_str),
            Some("9999-12-31")
        );
    }

    fn typed(anchor_name: &str, at: &str) -> RecordedAction {
        RecordedAction {
            app_id: "qq".to_string(),
            anchor: SemanticAnchor {
                app_id: Some("qq".to_string()),
                role: Some("edit".to_string()),
                name: anchor_name.to_string(),
                parent: None,
                element_id: None,
            },
            action_type: ActionType::Type,
            value_masked: true,
            sensitive: false,
            at: at.to_string(),
        }
    }

    #[test]
    fn generalizes_repeated_typed_anchor_to_variable() {
        let samples = vec![
            typed("搜索输入", "t1"),
            typed("搜索输入", "t2"),
            RecordedAction {
                app_id: "qq".to_string(),
                anchor: SemanticAnchor {
                    app_id: Some("qq".to_string()),
                    role: Some("button".to_string()),
                    name: "发送按钮".to_string(),
                    parent: None,
                    element_id: None,
                },
                action_type: ActionType::Click,
                value_masked: true,
                sensitive: false,
                at: "t3".to_string(),
            },
        ];
        let graph = generalize_to_graph(&samples).unwrap();
        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.variables(), vec!["value"]);
        assert!(graph.validate().is_ok());
    }

    #[test]
    fn generalize_traces_infers_variable_from_consistent_type_position() {
        let trace = |value_at: &str| {
            vec![
                typed("搜索输入", value_at),
                RecordedAction {
                    app_id: "qq".to_string(),
                    anchor: SemanticAnchor {
                        app_id: Some("qq".to_string()),
                        role: Some("button".to_string()),
                        name: "发送按钮".to_string(),
                        parent: None,
                        element_id: None,
                    },
                    action_type: ActionType::Click,
                    value_masked: true,
                    sensitive: false,
                    at: format!("{value_at}-click"),
                },
            ]
        };
        let traces = vec![trace("t1"), trace("t2"), trace("t3")];
        let graph = generalize_traces(&traces).unwrap();
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.variables(), vec!["value"]);
        assert!(graph.validate().is_ok());
        assert_eq!(graph.nodes[0].anchor.name, "搜索输入");
        assert_eq!(graph.nodes[1].anchor.name, "发送按钮");
    }

    #[test]
    fn generalize_traces_aligns_traces_with_length_variation() {
        // 第二条轨迹多一次无关点击，仍应对齐到骨干并生成合法图。
        let trace_a = vec![
            typed("搜索输入", "t1"),
            RecordedAction {
                app_id: "qq".to_string(),
                anchor: SemanticAnchor {
                    app_id: Some("qq".to_string()),
                    role: Some("button".to_string()),
                    name: "发送按钮".to_string(),
                    parent: None,
                    element_id: None,
                },
                action_type: ActionType::Click,
                value_masked: true,
                sensitive: false,
                at: "t1-click".to_string(),
            },
        ];
        let mut trace_b = trace_a.clone();
        trace_b.insert(
            1,
            RecordedAction {
                app_id: "qq".to_string(),
                anchor: SemanticAnchor {
                    app_id: Some("qq".to_string()),
                    role: Some("button".to_string()),
                    name: "无关按钮".to_string(),
                    parent: None,
                    element_id: None,
                },
                action_type: ActionType::Click,
                value_masked: true,
                sensitive: false,
                at: "t2-extra".to_string(),
            },
        );
        let graph = generalize_traces(&[trace_a, trace_b.clone(), trace_b]).unwrap();
        assert!(graph.validate().is_ok());
        assert_eq!(graph.nodes[0].anchor.name, "搜索输入");
        assert!(graph.variables().contains(&"value".to_string()));
    }

    #[test]
    fn candidate_eligible_enforces_count_and_success_rate() {
        let trace = || vec![typed("搜索输入", "t")];
        assert!(candidate_eligible(&[], &[]).is_err());
        assert!(candidate_eligible(&[trace(), trace()], &[]).is_err());
        assert!(candidate_eligible(&[trace(), trace(), trace()], &[]).is_ok());

        // 3 条轨迹 2 次成功 = 66.7% < 80%，拒绝。
        assert!(candidate_eligible(&[trace(), trace(), trace()], &[true, true, false]).is_err());
        // 5 条轨迹 4 次成功 = 80%，通过。
        assert!(candidate_eligible(
            &[trace(), trace(), trace(), trace(), trace()],
            &[true, true, true, true, false]
        )
        .is_ok());
    }

    #[test]
    fn generalize_traces_rejects_sensitive_or_empty_traces() {
        assert!(generalize_traces(&[vec![], vec![]]).is_err());
        let mut sensitive = typed("搜索输入", "t");
        sensitive.sensitive = true;
        assert!(generalize_traces(&[vec![sensitive], vec![typed("搜索输入", "t")]]).is_err());
    }

    #[test]
    fn pipeline_sinks_skill_package() {
        let root =
            std::env::temp_dir().join(format!("owo-learn-pipeline-{}", uuid::Uuid::new_v4()));
        let mut pipeline = LearnPipeline::new(root.join("skills").join("user"));
        pipeline.recorder.start();
        pipeline.recorder.record(typed("搜索输入", "t1")).unwrap();
        pipeline
            .recorder
            .record(RecordedAction {
                app_id: "qq".to_string(),
                anchor: SemanticAnchor {
                    app_id: Some("qq".to_string()),
                    role: Some("button".to_string()),
                    name: "发送按钮".to_string(),
                    parent: None,
                    element_id: None,
                },
                action_type: ActionType::Click,
                value_masked: true,
                sensitive: false,
                at: "t2".to_string(),
            })
            .unwrap();
        pipeline.stop_recording();
        let package = pipeline
            .sink_skill(
                "send-file",
                vec!["qq".to_string()],
                Sensitivity::Low,
                "在 QQ 发送文件",
            )
            .unwrap();
        assert!(package.validate().is_ok());
        assert_eq!(package.manifest.name, "send-file");
        assert_eq!(pipeline.store.list().unwrap(), vec!["send-file"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn recorded_actions_from_sequence_parses_click_and_type() {
        let sequence = vec![
            "click:发送".to_string(),
            "type:输入消息".to_string(),
            "搜索".to_string(),
        ];
        let actions = recorded_actions_from_sequence("qq", &sequence);
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0].action_type, ActionType::Click);
        assert_eq!(actions[0].anchor.name, "发送");
        assert_eq!(actions[0].anchor.role.as_deref(), Some("button"));
        assert_eq!(actions[1].action_type, ActionType::Type);
        assert!(actions[1].value_masked);
        assert_eq!(actions[2].action_type, ActionType::Click);
        assert_eq!(actions[2].anchor.name, "搜索");

        // 空项与未知前缀按 click 处理/跳过空锚点。
        let actions = recorded_actions_from_sequence(
            "qq",
            &["click:".to_string(), "shortcut:ctrl+a".to_string()],
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].anchor.name, "ctrl+a");
        assert_eq!(actions[0].action_type, ActionType::Click);
    }

    #[test]
    fn sink_from_actions_confirms_suggestion_into_active_package() {
        let root = std::env::temp_dir().join(format!("owo-learn-suggest-{}", uuid::Uuid::new_v4()));
        let pipeline = LearnPipeline::new(root.join("skills").join("user"));
        let sequence = vec![
            "click:搜索".to_string(),
            "type:输入消息".to_string(),
            "click:发送".to_string(),
        ];
        let samples = recorded_actions_from_sequence("qq", &sequence);
        let package = pipeline
            .sink_from_actions(
                "proactive-reply",
                vec!["qq".to_string()],
                Sensitivity::Low,
                "重复回复流程，用户确认沉淀",
                samples,
            )
            .expect("沉淀成功");
        assert!(package.validate().is_ok());
        assert_eq!(package.manifest.name, "proactive-reply");
        assert_eq!(pipeline.store.list().unwrap(), vec!["proactive-reply"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn decide_learn_removes_suggestion_from_list() {
        let mut engine = ProactiveEngine::new(ProactiveSettings {
            enabled: true,
            weekly_threshold: 5,
            daily_threshold: 1,
            similarity: 1.0,
            cooldown_hours: 0,
            daily_cap: 10,
            auto_silence_days: 30,
        });
        let actions = vec!["click:发送".to_string()];
        let suggestion = engine
            .observe("qq", actions)
            .expect("daily threshold 1 应产生建议");
        assert_eq!(engine.suggestions().len(), 1);
        engine
            .decide(&suggestion.id, SuggestionAction::Learn)
            .expect("确认学习");
        assert!(engine.suggestions().is_empty());
    }
}
