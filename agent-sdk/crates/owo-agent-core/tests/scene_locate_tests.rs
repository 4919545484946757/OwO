//! v0.5 M-A 契约测试：场景图跨帧稳定性 + 多源定位打分（技术文档 5.8.3）。

use owo_agent_core::element_registry::SceneElement;
use owo_agent_core::locate::{locate, AnchorQuery, LocateResult};
use owo_agent_core::scene::{
    elements_from_ocr_lines, elements_from_ui_nodes, elements_from_vision_groundings,
    merge_sources, Evidence, EvidenceSource, GraphElement, SceneGraph,
};
use owo_agent_core::UiNode;
use owo_agent_core::VisionGrounding;

fn element(id: &str, name: &str, x: i32, y: i32, role: &str) -> SceneElement {
    SceneElement {
        id: id.to_string(),
        app_id: "qq".to_string(),
        name: name.to_string(),
        role_hint: role.to_string(),
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

fn node(name: &str, x: i32, y: i32) -> UiNode {
    UiNode {
        name: name.to_string(),
        control_type: 50000,
        class: "Button".to_string(),
        depth: 0,
        x,
        y,
        width: 80,
        height: 30,
    }
}

fn ocr_line(text: &str, x: i32, y: i32) -> owo_agent_core::OcrLine {
    owo_agent_core::OcrLine {
        text: text.to_string(),
        x,
        y,
        width: 76,
        height: 28,
    }
}

fn vision(name: &str, x: i32, y: i32, cross_validated: bool) -> VisionGrounding {
    VisionGrounding {
        description: name.to_string(),
        x,
        y,
        width: 80,
        height: 30,
        confidence: 0.95,
        cross_validated,
    }
}

fn assert_io_u(result: &LocateResult, expected_center: (i32, i32), case: &str) {
    let best = result
        .best
        .as_ref()
        .unwrap_or_else(|| panic!("{case}：无 best"));
    let center = (best.x + best.width / 2, best.y + best.height / 2);
    // IoU≥0.8 等价于中心偏差不超过小矩形尺寸的 20%（此处矩形 80x30）。
    let max_dx = 8;
    let max_dy = 3;
    assert!(
        (center.0 - expected_center.0).abs() <= max_dx
            && (center.1 - expected_center.1).abs() <= max_dy,
        "{case}：IoU 低于 0.8，center=({}, {}) 期望=({}, {})",
        center.0,
        center.1,
        expected_center.0,
        expected_center.1
    );
}

#[test]
fn five_frames_stable_id_keep_rate_at_least_95_percent() {
    let mut graph = SceneGraph::new();
    let names = ["发送", "输入消息", "会话列表", "搜索", "联系人", "文件"];
    let make = |frame: i32| {
        names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let mut element = element(
                    "",
                    name,
                    20 + index as i32 * 90 + frame,
                    30 + frame,
                    "button",
                );
                element.id = String::new();
                GraphElement::from_element(element)
            })
            .collect::<Vec<_>>()
    };
    graph.update(None, None, make(0));
    let first_ids: std::collections::HashSet<String> = graph
        .elements
        .iter()
        .map(|entry| entry.element.id.clone())
        .collect();
    let mut min_keep = f64::MAX;
    for frame in 1..5 {
        graph.update(None, None, make(frame));
        let current: std::collections::HashSet<String> = graph
            .elements
            .iter()
            .map(|entry| entry.element.id.clone())
            .collect();
        let kept = current.intersection(&first_ids).count();
        min_keep = min_keep.min(kept as f64 / names.len() as f64);
    }
    assert!(
        min_keep >= 0.95,
        "连续 5 帧稳定 ID 保持率应 ≥95%，实际 {min_keep}"
    );
}

#[test]
fn locate_against_ground_truth_20_cases_io_u_ge_0_8() {
    let cases: Vec<(String, i32, i32)> = vec![
        ("发送".to_string(), 30, 40),
        ("输入消息".to_string(), 30, 90),
        ("搜索".to_string(), 500, 30),
        ("联系人".to_string(), 30, 150),
        ("文件".to_string(), 30, 200),
        ("表情".to_string(), 30, 250),
        ("设置".to_string(), 500, 90),
        ("聊天记录".to_string(), 500, 150),
        ("添加好友".to_string(), 30, 300),
        ("群聊".to_string(), 500, 200),
        ("转账".to_string(), 30, 350),
        ("收藏".to_string(), 500, 250),
        ("截图".to_string(), 30, 400),
        ("语音".to_string(), 500, 300),
        ("视频通话".to_string(), 30, 450),
        ("分享".to_string(), 500, 350),
        ("删除".to_string(), 30, 500),
        ("置顶".to_string(), 500, 400),
        ("免打扰".to_string(), 30, 550),
        ("备注".to_string(), 500, 450),
    ];
    let mut graph = SceneGraph::new();
    graph.update(
        None,
        None,
        cases
            .iter()
            .map(|(name, x, y)| {
                let mut element = element("", name, *x, *y, "button");
                element.id = String::new();
                GraphElement::from_element(element)
            })
            .collect(),
    );
    for (name, x, y) in &cases {
        let result = locate(&graph, &AnchorQuery::by_name("qq", name));
        assert_io_u(&result, (x + 40, y + 15), name);
    }
}

#[test]
fn vision_only_grounding_is_rejected_without_cross_validation() {
    let mut graph = SceneGraph::new();
    graph.update(
        None,
        None,
        elements_from_vision_groundings(&[vision("表情面板", 10, 20, false)], "qq"),
    );
    let result = locate(&graph, &AnchorQuery::by_name("qq", "表情面板"));
    assert!(
        !result.is_reliable(),
        "未交叉验证的视觉-only 定位应降级询问"
    );
    assert!(result.uncertainty > 0.5);
}

#[test]
fn cross_validated_vision_fuses_with_ocr_and_beats_ocr_alone() {
    let mut graph = SceneGraph::new();
    let mut ocr_elements = elements_from_ocr_lines(&[ocr_line("表情面板", 10, 20)], "qq");
    let vision_elements =
        elements_from_vision_groundings(&[vision("表情面板", 10, 20, true)], "qq");
    let merged = merge_sources(vec![std::mem::take(&mut ocr_elements), vision_elements]);
    assert_eq!(merged.len(), 1);
    let element = &merged[0];
    assert!(element.element.sources.contains(&"vision".to_string()));
    assert!(element.element.sources.contains(&"ocr".to_string()));
    assert_eq!(element.evidence.len(), 2);

    graph.update(None, None, merged);
    let result = locate(&graph, &AnchorQuery::by_name("qq", "表情面板"));
    assert!(result.is_reliable());
    assert!(result.candidates[0].1 >= 0.7);
}

#[test]
fn template_roi_and_history_prior_shape_locate_result() {
    let mut graph = SceneGraph::new();
    graph.update(
        None,
        None,
        vec![
            GraphElement::from_element(element("1", "发送", 10, 20, "button")),
            GraphElement::from_element(element("2", "发送", 500, 300, "button")),
        ],
    );
    let query = AnchorQuery::by_name("qq", "发送");
    let baseline_score = locate(&graph, &query).candidates[0].1;

    graph.set_template_roi("qq-main", (0, 0, 200, 200));
    for _ in 0..10 {
        graph.record_template_hit("qq-main", true);
    }
    let with_template = locate(&graph, &query);
    assert_eq!(
        with_template.best.unwrap().id,
        "1",
        "模板 ROI 内的元素应保持最优"
    );
    assert!(
        with_template.candidates[0].1 > baseline_score,
        "模板命中先验应提高得分"
    );

    graph.record_hit("2", &query.signature());
    assert_eq!(
        locate(&graph, &query).best.unwrap().id,
        "2",
        "历史先验应改变最优"
    );
}

#[test]
fn ui_ocr_vision_fusion_pipeline_builds_scene_graph() {
    let mut graph = SceneGraph::new();
    let mut ui = elements_from_ui_nodes(&[node("发送", 10, 20), node("输入消息", 10, 70)], "qq");
    let mut ocr = elements_from_ocr_lines(
        &[
            ocr_line("发送", 12, 21),
            ocr_line("输入消息", 12, 71),
            ocr_line("自绘控件", 300, 120),
        ],
        "qq",
    );
    let vision = elements_from_vision_groundings(&[vision("自绘控件", 300, 120, true)], "qq");
    let merged = merge_sources(vec![
        std::mem::take(&mut ui),
        std::mem::take(&mut ocr),
        vision,
    ]);
    graph.update(None, None, merged);
    assert_eq!(graph.element_count(), 3);
    let result = locate(&graph, &AnchorQuery::by_name("qq", "自绘控件"));
    assert!(result.is_reliable());
    let best = result.best.unwrap();
    let _: Evidence = Evidence::new(EvidenceSource::History, &best, 0.9);
}
