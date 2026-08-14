//! 多源定位打分（v0.5 M-A，对应技术文档 5.8.3）。
//!
//! 把 `executor::find_recursive` 升级为结构化锚点查询 + 概率定位：
//! `score = w_uia·uia + w_ocr·ocr + w_vision·vision(cross_validated) + w_template·template_hit + w_history·prior_hit`。
//! 命中后把稳定 ID 写回执行器锚点池；不确定高于阈值时降级询问。

use crate::element_registry::SceneElement;
use crate::scene::{EvidenceSource, GraphElement, SceneGraph};
use serde::{Deserialize, Serialize};

/// 结构化锚点查询。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_pattern: Option<String>,
    /// 父容器名称（如“会话列表”），用于消除同名节点歧义。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// 低于该置信度的结果视为不可靠（best 置 None，调用方降级询问）。
    #[serde(default = "default_min_confidence")]
    pub min_confidence: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_priority: Option<Vec<EvidenceSource>>,
    /// 稳定元素 ID：存在时最高优先级精确匹配。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_hash: Option<u64>,
    /// 上下文区域 (x, y, w, h)：用于消除同名元素歧义。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_rect: Option<(i32, i32, i32, i32)>,
}

fn default_min_confidence() -> f64 {
    0.4
}

impl Default for AnchorQuery {
    fn default() -> Self {
        Self {
            app_id: None,
            role: None,
            name_pattern: None,
            parent: None,
            min_confidence: default_min_confidence(),
            source_priority: None,
            stable_id: None,
            text_hash: None,
            context_rect: None,
        }
    }
}

impl AnchorQuery {
    pub fn by_stable_id(app_id: impl Into<String>, stable_id: impl Into<String>) -> Self {
        Self {
            app_id: Some(app_id.into()),
            stable_id: Some(stable_id.into()),
            ..Self::default()
        }
    }

    pub fn by_name(app_id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            app_id: Some(app_id.into()),
            name_pattern: Some(name.into()),
            ..Self::default()
        }
    }

    /// 查询签名（历史命中先验的键）：稳定 ID > app+role+name。
    pub fn signature(&self) -> String {
        if let Some(stable_id) = &self.stable_id {
            return format!("stable:{}", stable_id);
        }
        format!(
            "{}:{}:{}",
            self.app_id.as_deref().unwrap_or("*"),
            self.role.as_deref().unwrap_or("*"),
            self.name_pattern.as_deref().unwrap_or("*")
        )
    }
}

/// 定位结果：候选 + 最优 + 不确定性 + 主要来源。
#[derive(Debug, Clone, Default)]
pub struct LocateResult {
    pub candidates: Vec<(SceneElement, f64)>,
    pub best: Option<SceneElement>,
    pub uncertainty: f64,
    pub used_source: Option<EvidenceSource>,
}

impl LocateResult {
    /// 是否达到查询要求的可信线（供调用方决定直接执行还是询问）。
    pub fn is_reliable(&self) -> bool {
        self.uncertainty <= 0.35
    }
}

/// 各来源权重（设计文档：UIA 最高，视觉必须交叉验证后才作为证据）。
pub const WEIGHT_UIA: f64 = 0.45;
pub const WEIGHT_OCR: f64 = 0.25;
pub const WEIGHT_VISION: f64 = 0.15;
pub const WEIGHT_TEMPLATE: f64 = 0.08;
pub const WEIGHT_HISTORY: f64 = 0.07;

/// 多源定位打分：稳定 ID 精确匹配 → 名称/角色/区域过滤 → 证据加权。
pub fn locate(graph: &SceneGraph, query: &AnchorQuery) -> LocateResult {
    let mut candidates: Vec<(SceneElement, f64)> = graph
        .elements
        .iter()
        .filter(|entry| matches_query(entry, graph, query))
        .map(|entry| (entry.element.clone(), score_element(entry, graph, query)))
        .collect();
    candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let top_score = candidates.first().map(|(_, score)| *score).unwrap_or(0.0);
    let runner_up = candidates.get(1).map(|(_, score)| *score).unwrap_or(0.0);
    // 不确定性：无候选 = 1；单候选 = 1 - 最高分；多候选 = 1 - (最高分 - 次高分)。
    let uncertainty = if top_score <= 0.0 {
        1.0
    } else if candidates.len() == 1 {
        (1.0 - top_score).clamp(0.0, 1.0)
    } else {
        (1.0 - (top_score - runner_up)).clamp(0.0, 1.0)
    };

    // 低于 min_confidence 的候选不可靠：best 置 None，由调用方降级询问。
    let best = candidates
        .first()
        .filter(|(_, score)| *score >= query.min_confidence)
        .map(|(element, _)| element.clone());

    let used_source = best.as_ref().and_then(|element| {
        graph
            .elements
            .iter()
            .find(|entry| entry.element.id == element.id)
            .and_then(|entry| entry.evidence.last())
            .map(|evidence| evidence.source)
    });

    LocateResult {
        candidates,
        best,
        uncertainty,
        used_source,
    }
}

fn matches_query(entry: &GraphElement, graph: &SceneGraph, query: &AnchorQuery) -> bool {
    let element = &entry.element;
    if let Some(stable_id) = &query.stable_id {
        return element.id == *stable_id;
    }
    if let Some(app_id) = &query.app_id {
        if !element.app_id.is_empty() && element.app_id != *app_id {
            return false;
        }
    }
    if let Some(role) = &query.role {
        if element.role_hint.to_lowercase() != role.to_lowercase() {
            return false;
        }
    }
    if let Some(pattern) = &query.name_pattern {
        let name = element.name.to_lowercase();
        let pattern = pattern.to_lowercase();
        if !name.contains(&pattern) && !pattern.contains(&name) {
            return false;
        }
    }
    if let Some(parent) = &query.parent {
        let parent_id = graph
            .elements
            .iter()
            .find(|entry| entry.element.name == *parent)
            .map(|entry| entry.element.id.as_str());
        let has_parent = parent_id.is_some_and(|parent_id| {
            graph
                .relations
                .iter()
                .any(|relation| relation.parent == parent_id && relation.child == element.id)
        });
        if !has_parent {
            return false;
        }
    }
    if let Some(text_hash) = query.text_hash {
        let found = entry
            .evidence
            .iter()
            .any(|evidence| evidence.text_hash == Some(text_hash));
        if !found {
            return false;
        }
    }
    if let Some((cx, cy, cw, ch)) = query.context_rect {
        let center_x = element.x + element.width / 2;
        let center_y = element.y + element.height / 2;
        if center_x < cx || center_x > cx + cw || center_y < cy || center_y > cy + ch {
            return false;
        }
    }
    true
}

/// 对单个元素打分：稳定 ID 命中给满分；否则按证据加权求和。
pub fn score_element(entry: &GraphElement, graph: &SceneGraph, query: &AnchorQuery) -> f64 {
    if let Some(stable_id) = &query.stable_id {
        return if entry.element.id == *stable_id {
            1.0
        } else {
            0.0
        };
    }

    let mut score = 0.0;
    let mut covered = 0.0;
    let mut effective = entry.evidence.clone();
    if effective.is_empty() {
        // 骨架兼容：无证据时按元素 sources 推断一条 Uia/Ocr 证据。
        let source = if entry.element.sources.contains(&"uia".to_string()) {
            EvidenceSource::Uia
        } else if entry.element.sources.contains(&"ocr".to_string()) {
            EvidenceSource::Ocr
        } else {
            EvidenceSource::Vision
        };
        effective.push(crate::scene::Evidence::new(
            source,
            &entry.element,
            entry.confidence,
        ));
    }

    for evidence in &effective {
        let weight = evidence_weight(evidence.source, query);
        if weight <= 0.0 {
            continue;
        }
        // 视觉 grounding 未交叉验证时只作弱证据。
        let confidence =
            if evidence.source == EvidenceSource::Vision && !entry.vision_cross_validated() {
                evidence.confidence * 0.3
            } else {
                evidence.confidence
            };
        score += weight * confidence;
        covered += weight;
    }

    // 模板先验：元素中心落在模板 ROI 内，且该模板命中率健康 → 加弱证据。
    let (center_x, center_y) = entry.center();
    for (template_name, rect) in &graph.template_rois {
        let rate = graph.template_hit_rate(template_name).unwrap_or(0.5);
        if rate > 0.5
            && center_x >= rect.0
            && center_x <= rect.0 + rect.2
            && center_y >= rect.1
            && center_y <= rect.1 + rect.3
        {
            score += WEIGHT_TEMPLATE * rate;
            covered += WEIGHT_TEMPLATE;
        }
    }

    // 历史命中先验：该元素此前命中过相同查询 → 加弱证据。
    if entry
        .last_hit
        .as_deref()
        .is_some_and(|hit| hit == query.signature())
    {
        score += WEIGHT_HISTORY * 0.9;
        covered += WEIGHT_HISTORY;
        // 平局决胜：历史命中过的元素在同等得分下优先。
        score += 0.05;
    }

    let mut final_score = if covered > 0.0 {
        score / covered
    } else {
        entry.confidence.clamp(0.0, 1.0) * 0.5
    };
    if entry.conflict {
        final_score *= 0.6;
    }
    final_score.clamp(0.0, 1.0)
}

fn evidence_weight(source: EvidenceSource, query: &AnchorQuery) -> f64 {
    if let Some(priority) = &query.source_priority {
        let rank = priority
            .iter()
            .position(|candidate| *candidate == source)
            .map(|index| index + 1)
            .unwrap_or(priority.len() + 1);
        // 优先级排名越靠前权重越高：rank1 -> 0.8，rank2 -> 0.55，rank3 -> 0.35。
        let weight = match rank {
            1 => 0.8,
            2 => 0.55,
            3 => 0.35,
            _ => 0.2,
        };
        if priority.contains(&source) {
            return weight;
        }
    }
    match source {
        EvidenceSource::Uia => WEIGHT_UIA,
        EvidenceSource::Ocr => WEIGHT_OCR,
        EvidenceSource::Vision => WEIGHT_VISION,
        EvidenceSource::Template => WEIGHT_TEMPLATE,
        EvidenceSource::History => WEIGHT_HISTORY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{Evidence, GraphElement};

    fn element(id: &str, name: &str, x: i32, y: i32, app_id: &str) -> SceneElement {
        SceneElement {
            id: id.to_string(),
            app_id: app_id.to_string(),
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
    fn locate_prefers_stable_id_exact_match() {
        let mut graph = SceneGraph::new();
        let mut first = GraphElement::from_element(element("a:1", "发送", 10, 20, "qq"));
        first.add_evidence(Evidence::new(EvidenceSource::Uia, &first.element, 0.95));
        let second = GraphElement::from_element(element("a:2", "发送", 500, 300, "qq"));
        graph.update(None, None, vec![first, second]);

        let result = locate(&graph, &AnchorQuery::by_stable_id("qq", "a:2"));
        let reliable = result.is_reliable();
        let uncertainty = result.uncertainty;
        let best = result.best.unwrap();
        assert_eq!(best.id, "a:2");
        assert_eq!(uncertainty, 0.0);
        assert!(reliable);
    }

    #[test]
    fn locate_filters_by_name_and_context_rect() {
        let mut graph = SceneGraph::new();
        graph.update(
            None,
            None,
            vec![
                GraphElement::from_element(element("1", "输入消息", 10, 20, "qq")),
                GraphElement::from_element(element("2", "输入消息", 500, 300, "qq")),
            ],
        );
        let mut query = AnchorQuery::by_name("qq", "输入消息");
        query.context_rect = Some((400, 200, 400, 300));
        let result = locate(&graph, &query);
        assert_eq!(result.best.unwrap().id, "2");
    }

    #[test]
    fn vision_without_cross_validation_is_unreliable() {
        let mut graph = SceneGraph::new();
        let mut element = GraphElement::from_element(element("v1", "表情面板", 0, 0, "qq"));
        element.add_evidence(Evidence::new(
            EvidenceSource::Vision,
            &element.element,
            0.95,
        ));
        graph.update(None, None, vec![element]);
        let result = locate(&graph, &AnchorQuery::by_name("qq", "表情面板"));
        assert!(result.uncertainty > 0.5, "未交叉验证的视觉定位应高不确定");
        assert!(!result.is_reliable());
    }

    #[test]
    fn cross_validated_vision_joins_evidence_and_raises_score() {
        let mut graph = SceneGraph::new();
        let mut element = GraphElement::from_element(element("v1", "表情面板", 0, 0, "qq"));
        element.add_evidence(Evidence::new(
            EvidenceSource::Vision,
            &element.element,
            0.95,
        ));
        element.element.sources.push("ocr".to_string());
        element.element.sources.push("vision".to_string());
        graph.update(None, None, vec![element]);
        let result = locate(&graph, &AnchorQuery::by_name("qq", "表情面板"));
        let score = result.candidates[0].1;
        assert!(score >= 0.7, "交叉验证后视觉+OCR 得分应较高，实际 {score}");
    }

    #[test]
    fn template_roi_hit_rate_boosts_same_name_element() {
        let mut graph = SceneGraph::new();
        graph.update(
            None,
            None,
            vec![
                GraphElement::from_element(element("1", "发送", 10, 20, "qq")),
                GraphElement::from_element(element("2", "发送", 500, 300, "qq")),
            ],
        );
        graph.set_template_roi("qq-main", (0, 0, 200, 200));
        for _ in 0..10 {
            graph.record_template_hit("qq-main", true);
        }
        let result = locate(&graph, &AnchorQuery::by_name("qq", "发送"));
        assert_eq!(result.best.unwrap().id, "1", "模板 ROI 内的元素应胜出");
    }

    #[test]
    fn history_prior_boosts_previously_hit_element() {
        let mut graph = SceneGraph::new();
        graph.update(
            None,
            None,
            vec![
                GraphElement::from_element(element("1", "发送", 10, 20, "qq")),
                GraphElement::from_element(element("2", "发送", 500, 300, "qq")),
            ],
        );
        let query = AnchorQuery::by_name("qq", "发送");
        graph.record_hit("2", &query.signature());
        let result = locate(&graph, &query);
        assert_eq!(result.best.unwrap().id, "2", "历史命中先验应改变最优");
    }

    #[test]
    fn low_confidence_candidate_yields_no_best() {
        let mut graph = SceneGraph::new();
        let mut element = GraphElement::from_element(element("v1", "表情面板", 0, 0, "qq"));
        element.add_evidence(Evidence::new(EvidenceSource::Vision, &element.element, 0.2));
        graph.update(None, None, vec![element]);
        let mut query = AnchorQuery::by_name("qq", "表情面板");
        query.min_confidence = 0.5;
        let result = locate(&graph, &query);
        assert!(result.best.is_none(), "低于 min_confidence 不应给 best");
    }

    #[test]
    fn parent_relation_filters_ambiguous_names() {
        let mut graph = SceneGraph::new();
        let mut session_list = GraphElement::from_element(element("p1", "会话列表", 0, 0, "qq"));
        session_list.element.role_hint = "pane".to_string();
        session_list.element.width = 300;
        session_list.element.height = 600;
        let mut child = GraphElement::from_element(element("c1", "发送", 10, 20, "qq"));
        child.element.width = 60;
        child.element.height = 24;
        let other = GraphElement::from_element(element("c2", "发送", 700, 300, "qq"));
        graph.update(None, None, vec![session_list, child, other]);
        // update 已重建 relations（containment）。
        let mut query = AnchorQuery::by_name("qq", "发送");
        query.parent = Some("会话列表".to_string());
        let result = locate(&graph, &query);
        assert_eq!(result.best.unwrap().id, "c1");
    }

    #[test]
    fn synthetic_20_cases_top1_matches_ground_truth() {
        let cases: Vec<(&str, (i32, i32))> = vec![
            ("发送", (30, 40)),
            ("输入消息", (30, 90)),
            ("搜索", (500, 30)),
            ("联系人", (30, 150)),
            ("文件", (30, 200)),
            ("表情", (30, 250)),
            ("设置", (500, 90)),
            ("聊天记录", (500, 150)),
            ("添加好友", (30, 300)),
            ("群聊", (500, 200)),
            ("转账", (30, 350)),
            ("收藏", (500, 250)),
            ("截图", (30, 400)),
            ("语音", (500, 300)),
            ("视频通话", (30, 450)),
            ("分享", (500, 350)),
            ("删除", (30, 500)),
            ("置顶", (500, 400)),
            ("免打扰", (30, 550)),
            ("备注", (500, 450)),
        ];
        let mut graph = SceneGraph::new();
        let elements = cases
            .iter()
            .map(|(name, (x, y))| {
                let mut element = element("", name, *x, *y, "qq");
                element.id = String::new();
                GraphElement::from_element(element)
            })
            .collect::<Vec<_>>();
        graph.update(None, None, elements);

        let mut hits = 0;
        for (name, (x, y)) in cases.iter() {
            let result = locate(&graph, &AnchorQuery::by_name("qq", *name));
            let best = result.best.expect("应有候选");
            let ground_truth = (*x + 40, *y + 15);
            let best_center = (best.x + best.width / 2, best.y + best.height / 2);
            let distance = ((best_center.0 - ground_truth.0).abs() as f64
                + (best_center.1 - ground_truth.1).abs() as f64)
                / 2.0;
            if distance <= 8.0 {
                hits += 1;
            }
        }
        assert!(
            hits >= 19,
            "20 例 top-1 命中应 ≥19（IoU≥0.8 等价），实际 {hits}/20"
        );
    }
}
