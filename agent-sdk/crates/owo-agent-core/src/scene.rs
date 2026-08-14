//! 统一场景图（v0.5 M-A，对应技术文档 5.8.3）。
//!
//! 把 `SituationSnapshot`（应用级）、`SceneElement`（元素级）、`WindowTemplate`
//! （ROI 级）、`OcrSummary`（版面级）统一为跨帧世界模型：
//! 稳定元素 + 关系 + 多源证据 + 置信度，作为定位/执行/验证/学习的唯一事实来源。
//!
//! 融合规则：UIA 提供语义角色与几何（权重最高）；OCR 补自绘控件；视觉 grounding
//! 交叉验证后才加入；历史命中先验作为弱证据；同名但几何差异大的标记冲突并降置信度。

use crate::accessibility::UiNode;
use crate::element_registry::{SceneElement, VisionGrounding};
use crate::ocr::OcrLine;
use crate::perception::{ForegroundApp, TaskHypothesis};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

/// 证据来源：多源定位打分中的可加权信号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSource {
    Uia,
    Ocr,
    Vision,
    Template,
    History,
}

/// 单条元素证据：来源 + 几何 + 置信度 + 文本指纹。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub source: EvidenceSource,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub confidence: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_hash: Option<u64>,
}

impl Evidence {
    pub fn new(source: EvidenceSource, element: &SceneElement, confidence: f64) -> Self {
        Self {
            source,
            x: element.x,
            y: element.y,
            width: element.width,
            height: element.height,
            confidence,
            text_hash: None,
        }
    }

    pub fn with_text(
        source: EvidenceSource,
        element: &SceneElement,
        text: &str,
        confidence: f64,
    ) -> Self {
        let mut evidence = Self::new(source, element, confidence);
        evidence.text_hash = Some(text_hash(text));
        evidence
    }

    pub fn center(&self) -> (i32, i32) {
        (self.x + self.width / 2, self.y + self.height / 2)
    }

    pub fn overlaps(&self, other: &Evidence) -> bool {
        rect_overlap(
            (self.x, self.y, self.width, self.height),
            (other.x, other.y, other.width, other.height),
        ) > 0.0
    }
}

/// 场景图中的元素：注册表元素 + 多源证据 + 跨帧状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphElement {
    pub element: SceneElement,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    #[serde(default)]
    pub stale_frames: u32,
    /// 同名但几何差异大：冲突标记，参与定位时降置信度。
    #[serde(default)]
    pub conflict: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_hit: Option<String>,
}

fn default_confidence() -> f64 {
    0.5
}

impl GraphElement {
    pub fn from_element(element: SceneElement) -> Self {
        Self {
            confidence: element.confidence,
            element,
            evidence: Vec::new(),
            stale_frames: 0,
            conflict: false,
            last_hit: None,
        }
    }

    pub fn center(&self) -> (i32, i32) {
        (
            self.element.x + self.element.width / 2,
            self.element.y + self.element.height / 2,
        )
    }

    pub fn rect(&self) -> (i32, i32, i32, i32) {
        (
            self.element.x,
            self.element.y,
            self.element.width,
            self.element.height,
        )
    }

    /// 添加一条证据：同源去重（保留更高置信度），并刷新融合置信度。
    pub fn add_evidence(&mut self, evidence: Evidence) {
        if let Some(existing) = self
            .evidence
            .iter_mut()
            .find(|existing| existing.source == evidence.source)
        {
            if evidence.confidence > existing.confidence {
                *existing = evidence;
            }
        } else {
            self.evidence.push(evidence);
        }
        self.refresh_confidence();
    }

    /// 融合置信度：视觉未交叉验证时打折；多源取最大；冲突再打 0.6 折。
    fn refresh_confidence(&mut self) {
        let mut base: f64 = 0.0;
        for evidence in &self.evidence {
            let value =
                if evidence.source == EvidenceSource::Vision && !self.vision_cross_validated() {
                    evidence.confidence * 0.3
                } else {
                    evidence.confidence
                };
            base = base.max(value);
        }
        if base == 0.0 {
            base = self.element.confidence;
        }
        if self.conflict {
            base *= 0.6;
        }
        self.confidence = base.clamp(0.0, 1.0);
    }

    pub fn vision_cross_validated(&self) -> bool {
        self.element.sources.iter().any(|source| source == "ocr")
            && self.element.sources.iter().any(|source| source == "vision")
    }
}

/// 元素间关系：parent/contains/overlaps/occludes。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementRelation {
    pub parent: String,
    pub child: String,
    pub kind: String,
}

/// 实体状态：input_box.empty / window.focused 等可断言状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityState {
    pub name: String,
    pub value: String,
    pub confidence: f64,
}

/// 窗口级状态（几何/DPI/可见性/遮挡）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowState {
    pub hwnd: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub dpi: u32,
    pub visible: bool,
    pub occluded: bool,
}

/// 跨帧统一场景图：唯一事实来源。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SceneGraph {
    pub revision: u64,
    pub state_hash: u64,
    pub app: Option<ForegroundApp>,
    pub window: Option<WindowState>,
    pub elements: Vec<GraphElement>,
    pub relations: Vec<ElementRelation>,
    pub entities: HashMap<String, EntityState>,
    pub hypotheses: Vec<TaskHypothesis>,
    /// ROI 命中率：模板名 -> 平滑命中率（模板健康度，M-D 用）。
    pub template_hits: HashMap<String, f64>,
    /// 模板 ROI（屏幕坐标），供定位打分做区域内命中先验。
    #[serde(default)]
    pub template_rois: HashMap<String, (i32, i32, i32, i32)>,
    /// 最近一帧稳定 ID 保持率（0..=1）。
    #[serde(default)]
    pub last_keep_rate: f64,
    next_seq: u64,
}

impl SceneGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// 用新一帧元素更新场景图：稳定 ID 保持 + stale 淘汰 + 冲突标记 + 关系重建。
    ///
    /// 匹配规则：同 name + 同 role_hint + 中心距离 ≤ (48, 40)（与元素注册表一致）。
    /// 返回更新后的元素（含未匹配的 ghost 元素，stale_frames ≤ 3）。
    pub fn update(
        &mut self,
        app: Option<ForegroundApp>,
        window: Option<WindowState>,
        incoming: Vec<GraphElement>,
    ) -> Vec<GraphElement> {
        self.revision = self.revision.wrapping_add(1);
        self.app = app;
        self.window = window;

        let previous = std::mem::take(&mut self.elements);
        let mut used_prev: HashSet<String> = HashSet::new();
        let mut next: Vec<GraphElement> = Vec::with_capacity(incoming.len());
        let mut kept = 0usize;
        let mut seq = self.next_seq;
        let app_id = self
            .app
            .as_ref()
            .map(|app| app.id.clone())
            .unwrap_or_else(|| "unknown".to_string());

        for mut element in incoming {
            let (cx, cy) = element.center();
            let matched = previous
                .iter()
                .filter(|prev| {
                    !used_prev.contains(&prev.element.id)
                        && !prev.element.id.is_empty()
                        && prev.element.name == element.element.name
                        && prev.element.role_hint == element.element.role_hint
                })
                .min_by_key(|prev| {
                    let (px, py) = prev.center();
                    (px - cx).abs() + (py - cy).abs()
                });

            if let Some(prev) = matched {
                let (px, py) = prev.center();
                let near = (px - cx).abs() <= 48 && (py - cy).abs() <= 40;
                if near {
                    element.element.id = prev.element.id.clone();
                    element.element.app_id = prev.element.app_id.clone();
                    element.stale_frames = 0;
                    element.last_hit = prev.last_hit.clone();
                    used_prev.insert(element.element.id.clone());
                    kept += 1;
                } else {
                    if element.element.id.is_empty() {
                        seq += 1;
                        element.element.id =
                            format!("{app_id}:{}:{seq}", element.element.role_hint);
                        if element.element.app_id.is_empty() {
                            element.element.app_id = app_id.clone();
                        }
                    }
                    element.stale_frames = 0;
                }
            } else {
                if element.element.id.is_empty() {
                    seq += 1;
                    element.element.id = format!("{app_id}:{}:{seq}", element.element.role_hint);
                    if element.element.app_id.is_empty() {
                        element.element.app_id = app_id.clone();
                    }
                }
                element.stale_frames = 0;
            }
            next.push(element);
        }
        self.next_seq = seq;

        // 未匹配到的旧元素：stale 递增，超过 3 帧淘汰。
        for prev in previous
            .iter()
            .filter(|prev| !used_prev.contains(&prev.element.id))
        {
            let mut ghost = prev.clone();
            ghost.stale_frames += 1;
            if ghost.stale_frames <= 3 {
                next.push(ghost);
            }
        }

        mark_conflicts(&mut next);
        self.last_keep_rate = if next.is_empty() {
            1.0
        } else {
            kept as f64 / next.len().max(1) as f64
        };
        self.elements = next.clone();
        self.relations = build_relations(&next);
        self.state_hash = state_hash(&next);
        next
    }

    pub fn set_template_roi(&mut self, name: impl Into<String>, rect: (i32, i32, i32, i32)) {
        self.template_rois.insert(name.into(), rect);
    }

    /// 命中回调：写入历史命中先验（签名 = 查询标识）。
    pub fn record_hit(&mut self, stable_id: &str, signature: &str) {
        if let Some(element) = self
            .elements
            .iter_mut()
            .find(|element| element.element.id == stable_id)
        {
            element.last_hit = Some(signature.to_string());
            element.stale_frames = 0;
        }
    }

    pub fn add_entity(
        &mut self,
        name: impl Into<String>,
        value: impl Into<String>,
        confidence: f64,
    ) {
        let name = name.into();
        self.entities.insert(
            name.clone(),
            EntityState {
                name,
                value: value.into(),
                confidence,
            },
        );
    }

    pub fn entity(&self, name: &str) -> Option<&EntityState> {
        self.entities.get(name)
    }

    /// 记录一次模板命中/未命中（指数平滑，用于 M-D 模板健康度）。
    pub fn record_template_hit(&mut self, template: &str, hit: bool) {
        let rate = self
            .template_hits
            .entry(template.to_string())
            .or_insert(0.5);
        *rate = if hit { *rate * 0.8 + 0.2 } else { *rate * 0.8 };
    }

    pub fn template_hit_rate(&self, template: &str) -> Option<f64> {
        self.template_hits.get(template).copied()
    }

    pub fn mark_stale(&mut self) {
        for element in &mut self.elements {
            element.stale_frames += 1;
        }
        self.elements.retain(|element| element.stale_frames <= 3);
        self.state_hash = state_hash(&self.elements);
    }

    /// 当前稳定元素数（供稳定性指标统计）。
    pub fn element_count(&self) -> usize {
        self.elements.len()
    }

    pub fn element(&self, stable_id: &str) -> Option<&GraphElement> {
        self.elements
            .iter()
            .find(|element| element.element.id == stable_id)
    }

    /// 最近一帧稳定 ID 保持率（≥0.95 为 M-A 验收线）。
    pub fn stable_keep_rate(&self) -> f64 {
        self.last_keep_rate
    }
}

/// UIA 节点 → 场景元素（证据源 Uia，权重最高）。
pub fn elements_from_ui_nodes(nodes: &[UiNode], app_id: &str) -> Vec<GraphElement> {
    let now = chrono::Utc::now().to_rfc3339();
    nodes
        .iter()
        .filter(|node| !node.name.trim().is_empty() && node.width > 0 && node.height > 0)
        .map(|node| {
            let element = SceneElement {
                id: String::new(),
                app_id: app_id.to_string(),
                name: node.name.clone(),
                role_hint: role_hint_from_control_type(node.control_type),
                sources: vec!["uia".to_string()],
                x: node.x,
                y: node.y,
                width: node.width,
                height: node.height,
                confidence: 0.9,
                stale_frames: 0,
                updated_at: now.clone(),
            };
            let mut graph_element = GraphElement::from_element(element);
            graph_element.add_evidence(Evidence::new(
                EvidenceSource::Uia,
                &graph_element.element,
                0.9,
            ));
            graph_element
        })
        .collect()
}

/// OCR 行 → 场景元素（证据源 Ocr，补自绘控件）。
pub fn elements_from_ocr_lines(lines: &[OcrLine], app_id: &str) -> Vec<GraphElement> {
    let now = chrono::Utc::now().to_rfc3339();
    lines
        .iter()
        .filter(|line| !line.text.trim().is_empty())
        .map(|line| {
            let element = SceneElement {
                id: String::new(),
                app_id: app_id.to_string(),
                name: line.text.clone(),
                role_hint: role_hint_from_text(&line.text),
                sources: vec!["ocr".to_string()],
                x: line.x,
                y: line.y,
                width: line.width,
                height: line.height,
                confidence: 0.85,
                stale_frames: 0,
                updated_at: now.clone(),
            };
            let mut graph_element = GraphElement::from_element(element);
            let evidence = Evidence::with_text(
                EvidenceSource::Ocr,
                &graph_element.element,
                &line.text,
                0.85,
            );
            graph_element.add_evidence(evidence);
            graph_element
        })
        .collect()
}

/// 视觉 grounding → 场景元素（证据源 Vision；交叉验证后才全权重）。
pub fn elements_from_vision_groundings(
    groundings: &[VisionGrounding],
    app_id: &str,
) -> Vec<GraphElement> {
    let now = chrono::Utc::now().to_rfc3339();
    groundings
        .iter()
        .filter(|grounding| !grounding.description.trim().is_empty())
        .map(|grounding| {
            let sources = if grounding.cross_validated {
                vec!["vision".to_string(), "ocr".to_string()]
            } else {
                vec!["vision".to_string()]
            };
            let element = SceneElement {
                id: String::new(),
                app_id: app_id.to_string(),
                name: grounding.description.clone(),
                role_hint: role_hint_from_text(&grounding.description),
                sources,
                x: grounding.x,
                y: grounding.y,
                width: grounding.width,
                height: grounding.height,
                confidence: grounding.confidence.clamp(0.0, 1.0),
                stale_frames: 0,
                updated_at: now.clone(),
            };
            let mut graph_element = GraphElement::from_element(element);
            graph_element.add_evidence(Evidence::new(
                EvidenceSource::Vision,
                &graph_element.element,
                grounding.confidence.clamp(0.0, 1.0),
            ));
            graph_element
        })
        .collect()
}

/// 多源元素融合：同名 + 矩形重合的元素合并证据（UIA 几何优先）。
pub fn merge_sources(mut groups: Vec<Vec<GraphElement>>) -> Vec<GraphElement> {
    let mut merged: Vec<GraphElement> = Vec::new();
    for group in groups.drain(..) {
        for mut element in group {
            if let Some(existing) = merged.iter_mut().find(|existing| {
                let existing_area =
                    (existing.element.width * existing.element.height).max(1) as f64;
                let element_area = (element.element.width * element.element.height).max(1) as f64;
                existing.element.name == element.element.name
                    && rect_overlap(existing.rect(), element.rect())
                        > 0.2 * existing_area.min(element_area)
            }) {
                for source in element.element.sources.drain(..) {
                    if !existing.element.sources.contains(&source) {
                        existing.element.sources.push(source);
                    }
                }
                for evidence in element.evidence.drain(..) {
                    existing.add_evidence(evidence);
                }
                // UIA 几何优先：已有 UIA 时不覆盖。
                if !existing.element.sources.iter().any(|s| s == "uia") {
                    existing.element.x = element.element.x;
                    existing.element.y = element.element.y;
                    existing.element.width = element.element.width;
                    existing.element.height = element.element.height;
                }
            } else {
                merged.push(element);
            }
        }
    }
    merged
}

fn mark_conflicts(elements: &mut [GraphElement]) {
    for i in 0..elements.len() {
        for j in (i + 1)..elements.len() {
            let (a, b) = (&elements[i], &elements[j]);
            if a.element.name == b.element.name
                && a.element.role_hint == b.element.role_hint
                && a.element.id != b.element.id
            {
                let (ax, ay) = a.center();
                let (bx, by) = b.center();
                if (ax - bx).abs() > 120 || (ay - by).abs() > 100 {
                    elements[i].conflict = true;
                    elements[j].conflict = true;
                }
            }
        }
    }
}

fn build_relations(elements: &[GraphElement]) -> Vec<ElementRelation> {
    let mut relations = Vec::new();
    for i in 0..elements.len() {
        for j in 0..elements.len() {
            if i == j {
                continue;
            }
            let (a, b) = (&elements[i], &elements[j]);
            if a.element.id == b.element.id {
                continue;
            }
            let overlap = rect_overlap(a.rect(), b.rect());
            let a_area = (a.element.width * a.element.height).max(1) as f64;
            let b_area = (b.element.width * b.element.height).max(1) as f64;
            let min_area = a_area.min(b_area);
            // 大矩形包含小矩形（小矩形 ≥60% 面积被覆盖）。
            if overlap > 0.6 * min_area {
                let (parent, child) = if a_area >= b_area {
                    (a.element.id.clone(), b.element.id.clone())
                } else {
                    (b.element.id.clone(), a.element.id.clone())
                };
                relations.push(ElementRelation {
                    parent,
                    child,
                    kind: "contains".to_string(),
                });
            }
        }
    }
    relations
}

fn rect_overlap(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> f64 {
    let w = (a.0 + a.2).min(b.0 + b.2) - a.0.max(b.0);
    let h = (a.1 + a.3).min(b.1 + b.3) - a.1.max(b.1);
    if w <= 0 || h <= 0 {
        0.0
    } else {
        (w * h) as f64
    }
}

pub fn text_hash(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

fn role_hint_from_control_type(control_type: i32) -> String {
    // 与 element_registry 的映射保持一致：Button/Edit/ListItem 等常见类型。
    match control_type {
        50000 => "button".to_string(),
        50004 => "edit".to_string(),
        50007 => "list_item".to_string(),
        50002 => "window".to_string(),
        50003 => "pane".to_string(),
        50020 => "check_box".to_string(),
        50033 => "hyperlink".to_string(),
        _ => "generic".to_string(),
    }
}

fn role_hint_from_text(text: &str) -> String {
    let lower = text.to_lowercase();
    if lower.contains("发送") || lower.contains("搜索") || lower.contains("确定") {
        "button".to_string()
    } else if lower.contains("输入") || lower.contains("搜索框") || lower.contains("请输入")
    {
        "edit".to_string()
    } else {
        "generic".to_string()
    }
}

fn state_hash(elements: &[GraphElement]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for element in elements {
        element.element.id.hash(&mut hasher);
        element.element.x.hash(&mut hasher);
        element.element.y.hash(&mut hasher);
        element.element.width.hash(&mut hasher);
        element.element.height.hash(&mut hasher);
        element.element.name.hash(&mut hasher);
        element.conflict.hash(&mut hasher);
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(id: &str, name: &str, x: i32, y: i32) -> SceneElement {
        SceneElement {
            id: id.to_string(),
            app_id: "test".to_string(),
            name: name.to_string(),
            role_hint: "button".to_string(),
            sources: vec!["uia".to_string()],
            x,
            y,
            width: 80,
            height: 30,
            confidence: 0.9,
            stale_frames: 0,
            updated_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    #[test]
    fn scene_graph_update_tracks_revision_and_hash() {
        let mut graph = SceneGraph::new();
        let elements = vec![GraphElement::from_element(element("1", "发送", 10, 20))];
        let before = graph.state_hash;
        graph.update(None, None, elements);
        assert_eq!(graph.revision, 1);
        assert_ne!(graph.state_hash, before);
        assert_eq!(graph.element_count(), 1);
        assert!(graph.element("1").is_some());
    }

    #[test]
    fn template_hit_rate_smooths_toward_one() {
        let mut graph = SceneGraph::new();
        for _ in 0..20 {
            graph.record_template_hit("qq-main", true);
        }
        let rate = graph.template_hit_rate("qq-main").unwrap();
        assert!(rate > 0.9, "连续命中应接近 1，实际 {rate}");
        for _ in 0..20 {
            graph.record_template_hit("qq-main", false);
        }
        let rate = graph.template_hit_rate("qq-main").unwrap();
        assert!(rate < 0.1, "连续未命中应接近 0，实际 {rate}");
    }

    #[test]
    fn stable_ids_survive_small_drift_across_frames() {
        let mut graph = SceneGraph::new();
        let names = ["发送", "输入消息", "会话列表", "搜索"];
        let make = |frame: i32| {
            names
                .iter()
                .enumerate()
                .map(|(index, name)| {
                    let mut element =
                        element("", name, 20 + index as i32 * 100 + frame, 30 + frame);
                    element.id = String::new();
                    GraphElement::from_element(element)
                })
                .collect::<Vec<_>>()
        };
        graph.update(None, None, make(0));
        let first_ids: HashSet<String> = graph
            .elements
            .iter()
            .map(|element| element.element.id.clone())
            .collect();
        let mut min_keep = f64::MAX;
        for frame in 1..5 {
            graph.update(None, None, make(frame));
            let current: HashSet<String> = graph
                .elements
                .iter()
                .map(|element| element.element.id.clone())
                .collect();
            let kept = current.intersection(&first_ids).count();
            min_keep = min_keep.min(kept as f64 / names.len() as f64);
        }
        assert!(
            min_keep >= 0.95,
            "5 帧稳定 ID 保持率应 ≥95%，实际 {min_keep}"
        );
    }

    #[test]
    fn stale_elements_are_evicted_after_three_frames() {
        let mut graph = SceneGraph::new();
        graph.update(
            None,
            None,
            vec![GraphElement::from_element(element("1", "发送", 10, 20))],
        );
        assert_eq!(graph.element_count(), 1);
        for _ in 0..3 {
            graph.update(None, None, Vec::new());
        }
        assert_eq!(graph.element_count(), 1, "stale=3 仍保留");
        graph.update(None, None, Vec::new());
        assert_eq!(graph.element_count(), 0, "超过 3 帧应淘汰");
    }

    #[test]
    fn same_name_distant_elements_mark_conflict() {
        let mut graph = SceneGraph::new();
        graph.update(
            None,
            None,
            vec![
                GraphElement::from_element(element("", "发送", 10, 20)),
                GraphElement::from_element(element("", "发送", 900, 600)),
            ],
        );
        assert!(graph.elements.iter().all(|element| element.conflict));
    }

    #[test]
    fn merge_sources_combines_uia_and_ocr_evidence() {
        let mut uia = elements_from_ui_nodes(
            &[UiNode {
                name: "发送".to_string(),
                control_type: 50000,
                class: "Button".to_string(),
                depth: 0,
                x: 10,
                y: 20,
                width: 80,
                height: 30,
            }],
            "qq",
        );
        let mut ocr = elements_from_ocr_lines(
            &[OcrLine {
                text: "发送".to_string(),
                x: 12,
                y: 21,
                width: 76,
                height: 28,
            }],
            "qq",
        );
        let merged = merge_sources(vec![std::mem::take(&mut uia), std::mem::take(&mut ocr)]);
        assert_eq!(merged.len(), 1);
        assert!(merged[0].element.sources.contains(&"uia".to_string()));
        assert!(merged[0].element.sources.contains(&"ocr".to_string()));
        assert_eq!(merged[0].evidence.len(), 2);
    }
}
