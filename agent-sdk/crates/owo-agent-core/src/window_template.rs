//! 窗口模板（设计文档 M-A）：从 UIA 树提取固定布局应用的语义 ROI 集合，
//! 用于窗口级定位（会话列表/消息区/输入框/发送按钮），支持缓存与命中检测。

use crate::accessibility::UiNode;
use crate::ocr::{group_ocr_lines, OcrSummary};
use serde::{Deserialize, Serialize};
use std::path::Path;

const KNOWN_ROIS: &[&str] = &[
    "会话列表",
    "消息列表",
    "Rich Text Editor",
    "发送",
    "搜索",
    "表情",
    "文件",
    "红包",
    "聊天记录",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowRoi {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowTemplate {
    pub app_id: String,
    pub built_at: String,
    pub rois: Vec<WindowRoi>,
}

/// 从 UIA 树构建窗口模板：取已知语义 ROI 的首个匹配矩形。
pub fn build_template(app_id: &str, tree: &[UiNode]) -> WindowTemplate {
    let mut rois = Vec::new();
    for known in KNOWN_ROIS {
        if let Some(node) = tree
            .iter()
            .find(|node| node.name == *known && node.width > 0 && node.height > 0)
        {
            rois.push(WindowRoi {
                name: known.to_string(),
                x: node.x,
                y: node.y,
                width: node.width,
                height: node.height,
            });
        }
    }
    WindowTemplate {
        app_id: app_id.to_string(),
        built_at: chrono::Utc::now().to_rfc3339(),
        rois,
    }
}

/// 从 OCR 版面构建窗口模板（后台可用：PrintWindow + PP-OCRv6，不依赖 UIA 激活态）。
/// 识别“发送/输入框/搜索/表情/文件/红包”等语义 ROI。
pub fn build_template_from_ocr(app_id: &str, summary: &OcrSummary) -> WindowTemplate {
    let mut rois = Vec::new();
    for line in group_ocr_lines(&summary.boxes) {
        let name = if line.text.contains("发送") && line.text.chars().count() <= 8 {
            "发送"
        } else if line.text.contains("输入")
            && (line.text.contains("消息") || line.text.contains('…') || line.text.contains("..."))
        {
            "输入框"
        } else if line.text.contains("搜索") {
            "搜索"
        } else if line.text.contains("表情") {
            "表情"
        } else if line.text.contains("文件") {
            "文件"
        } else if line.text.contains("红包") {
            "红包"
        } else {
            continue;
        };
        if !rois.iter().any(|roi: &WindowRoi| roi.name == name) {
            rois.push(WindowRoi {
                name: name.to_string(),
                x: line.x,
                y: line.y,
                width: line.width,
                height: line.height,
            });
        }
    }
    WindowTemplate {
        app_id: app_id.to_string(),
        built_at: chrono::Utc::now().to_rfc3339(),
        rois,
    }
}

pub fn templates_dir(data_root: &Path) -> std::path::PathBuf {
    data_root.join("window-templates")
}

pub fn save_template(data_root: &Path, template: &WindowTemplate) -> Result<(), String> {
    let dir = templates_dir(data_root);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建窗口模板目录失败：{e}"))?;
    let path = dir.join(format!("{}.json", template.app_id));
    let content =
        serde_json::to_string_pretty(template).map_err(|e| format!("序列化模板失败：{e}"))?;
    std::fs::write(&path, content).map_err(|e| format!("写入模板失败：{e}"))
}

pub fn load_template(data_root: &Path, app_id: &str) -> Option<WindowTemplate> {
    let path = templates_dir(data_root).join(format!("{app_id}.json"));
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// 检测模板 ROI 在当前窗口树中的命中情况（名称匹配 + 位置容差）。
pub fn detect_template(template: &WindowTemplate, tree: &[UiNode]) -> serde_json::Value {
    let mut rois = Vec::new();
    let mut hit = 0;
    for roi in &template.rois {
        let matched = tree.iter().any(|node| {
            node.name == roi.name && (node.x - roi.x).abs() < 80 && (node.y - roi.y).abs() < 40
        });
        if matched {
            hit += 1;
        }
        rois.push(serde_json::json!({
            "name": roi.name,
            "matched": matched,
            "rect": [roi.x, roi.y, roi.width, roi.height],
        }));
    }
    serde_json::json!({
        "app_id": template.app_id,
        "total": template.rois.len(),
        "hit": hit,
        "rois": rois,
    })
}

/// OCR 版模板检测：当前窗口 OCR 行中心是否落在模板 ROI 内。
pub fn detect_template_ocr(template: &WindowTemplate, summary: &OcrSummary) -> serde_json::Value {
    let lines = group_ocr_lines(&summary.boxes);
    let mut rois = Vec::new();
    let mut hit = 0;
    for roi in &template.rois {
        let matched = lines.iter().any(|line| {
            let cx = line.x + line.width / 2;
            let cy = line.y + line.height / 2;
            cx >= roi.x - 20
                && cx <= roi.x + roi.width + 20
                && cy >= roi.y - 20
                && cy <= roi.y + roi.height + 20
        });
        if matched {
            hit += 1;
        }
        rois.push(serde_json::json!({
            "name": roi.name,
            "matched": matched,
            "rect": [roi.x, roi.y, roi.width, roi.height],
        }));
    }
    serde_json::json!({
        "app_id": template.app_id,
        "total": template.rois.len(),
        "hit": hit,
        "rois": rois,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_tree() -> Vec<UiNode> {
        vec![
            UiNode {
                name: "会话列表".into(),
                control_type: 0,
                class: String::new(),
                depth: 1,
                x: 902,
                y: 271,
                width: 250,
                height: 676,
            },
            UiNode {
                name: "消息列表".into(),
                control_type: 0,
                class: String::new(),
                depth: 1,
                x: 1152,
                y: 271,
                width: 700,
                height: 469,
            },
            UiNode {
                name: "Rich Text Editor".into(),
                control_type: 0,
                class: String::new(),
                depth: 1,
                x: 1152,
                y: 779,
                width: 700,
                height: 111,
            },
            UiNode {
                name: "发送".into(),
                control_type: 0,
                class: String::new(),
                depth: 1,
                x: 1754,
                y: 905,
                width: 28,
                height: 28,
            },
            UiNode {
                name: "搜索".into(),
                control_type: 0,
                class: String::new(),
                depth: 1,
                x: 939,
                y: 235,
                width: 163,
                height: 20,
            },
        ]
    }

    #[test]
    fn build_and_detect_template_round_trip() {
        let tree = fake_tree();
        let template = build_template("qq", &tree);
        assert_eq!(template.rois.len(), 5);
        assert_eq!(template.rois[0].name, "会话列表");
        let report = detect_template(&template, &tree);
        assert_eq!(report["hit"], 5);
        // 位置偏移后不再命中
        let shifted: Vec<UiNode> = tree
            .iter()
            .map(|node| UiNode {
                x: node.x + 200,
                y: node.y,
                ..node.clone()
            })
            .collect();
        let report = detect_template(&template, &shifted);
        assert_eq!(report["hit"], 0);
    }

    #[test]
    fn template_save_load_round_trip() {
        let dir = std::env::temp_dir().join(format!("owo-template-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let template = build_template("qq", &fake_tree());
        save_template(&dir, &template).expect("保存成功");
        let loaded = load_template(&dir, "qq").expect("加载成功");
        assert_eq!(loaded.rois.len(), template.rois.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_and_detect_template_from_ocr() {
        use crate::ocr::OcrBox;
        let summary = OcrSummary {
            chars: 4,
            text: "发送\n输入消息...".to_string(),
            boxes: vec![
                OcrBox {
                    text: "发送".into(),
                    x: 815,
                    y: 900,
                    width: 170,
                    height: 36,
                },
                OcrBox {
                    text: "输入消息...".into(),
                    x: 240,
                    y: 620,
                    width: 560,
                    height: 44,
                },
            ],
            provider: Some("paddle-v6".into()),
        };
        let template = build_template_from_ocr("qq", &summary);
        assert!(template.rois.iter().any(|roi| roi.name == "发送"));
        assert!(template.rois.iter().any(|roi| roi.name == "输入框"));
        let report = detect_template_ocr(&template, &summary);
        assert_eq!(report["hit"], 2);
    }
}
