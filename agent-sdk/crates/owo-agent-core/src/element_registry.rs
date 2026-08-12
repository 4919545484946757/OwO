//! 窗口元素注册表（设计文档 10.1）：UIA/OCR/视觉多源融合 → 稳定元素 ID → 跨帧跟踪与失效。
//!
//! 每帧把 UIA 节点与 OCR 行融合成 `SceneElement`，注册表按“名称+角色+位置邻近”匹配上一帧，
//! 保持稳定 ID；连续多帧未出现则标记 stale 后淘汰。视觉源（vision）预留，接入后同样参与融合。

use crate::accessibility::UiNode;
use crate::ocr::OcrLine;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceneElement {
    pub id: String,
    pub app_id: String,
    pub name: String,
    pub role_hint: String,
    pub sources: Vec<String>,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub confidence: f64,
    #[serde(default)]
    pub stale_frames: u32,
    pub updated_at: String,
}

impl SceneElement {
    fn center(&self) -> (i32, i32) {
        (self.x + self.width / 2, self.y + self.height / 2)
    }

    fn rect_overlaps(&self, other: &SceneElement) -> bool {
        let overlap_w = (self.x + self.width).min(other.x + other.width) - self.x.max(other.x);
        let overlap_h = (self.y + self.height).min(other.y + other.height) - self.y.max(other.y);
        overlap_w > 0
            && overlap_h > 0
            && (overlap_w * overlap_h) as f64 > 0.2 * (self.width * self.height).max(1) as f64
    }
}

#[derive(Debug, Clone, Default)]
pub struct ElementRegistry {
    apps: HashMap<String, HashMap<String, SceneElement>>,
    next_id: usize,
}

impl ElementRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 用新一帧元素更新注册表：保持稳定 ID，stale 递增，超过 3 帧淘汰。
    pub fn update(&mut self, app_id: &str, incoming: Vec<SceneElement>) -> Vec<SceneElement> {
        let entries = self.apps.entry(app_id.to_string()).or_default();
        let mut used: HashSet<String> = HashSet::new();
        for element in incoming {
            let (center_x, center_y) = element.center();
            let matched = entries.values_mut().find(|existing| {
                !used.contains(&existing.id)
                    && existing.name == element.name
                    && existing.role_hint == element.role_hint
                    && {
                        let (ex, ey) = existing.center();
                        (ex - center_x).abs() <= 48 && (ey - center_y).abs() <= 40
                    }
            });
            match matched {
                Some(existing) => {
                    existing.x = element.x;
                    existing.y = element.y;
                    existing.width = element.width;
                    existing.height = element.height;
                    existing.confidence = existing.confidence.max(element.confidence);
                    existing.stale_frames = 0;
                    existing.updated_at = element.updated_at;
                    for source in &element.sources {
                        if !existing.sources.contains(source) {
                            existing.sources.push(source.clone());
                        }
                    }
                    used.insert(existing.id.clone());
                }
                None => {
                    let id = format!("{}:{}:{}", app_id, element.role_hint, self.next_id);
                    self.next_id += 1;
                    let mut element = element;
                    element.id = id;
                    used.insert(element.id.clone());
                    entries.insert(element.id.clone(), element);
                }
            }
        }
        for element in entries.values_mut() {
            if !used.contains(&element.id) {
                element.stale_frames += 1;
            }
        }
        entries.retain(|_, element| element.stale_frames <= 3);
        entries.values().cloned().collect()
    }

    pub fn get(&self, app_id: &str, name: &str) -> Option<SceneElement> {
        self.apps
            .get(app_id)
            .and_then(|entries| entries.values().find(|element| element.name == name))
            .cloned()
    }

    pub fn list(&self, app_id: &str) -> Vec<SceneElement> {
        self.apps
            .get(app_id)
            .map(|entries| entries.values().cloned().collect())
            .unwrap_or_default()
    }
}

/// 把 UIA 节点与 OCR 行融合为统一 SceneElement 列表（同名且矩形重合的合并，UIA 优先几何）。
pub fn fuse_sources(uia: &[UiNode], lines: &[OcrLine]) -> Vec<SceneElement> {
    let mut elements: Vec<SceneElement> = Vec::new();
    let now = chrono::Utc::now().to_rfc3339();
    for node in uia {
        if node.name.trim().is_empty() || node.width <= 0 || node.height <= 0 {
            continue;
        }
        elements.push(SceneElement {
            id: String::new(),
            app_id: String::new(),
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
        });
    }
    for line in lines {
        if line.text.trim().is_empty() {
            continue;
        }
        let ocr_element = SceneElement {
            id: String::new(),
            app_id: String::new(),
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
        let merged = elements.iter_mut().find(|element| {
            element.name == ocr_element.name
                && element.role_hint == ocr_element.role_hint
                && element.rect_overlaps(&ocr_element)
        });
        match merged {
            Some(element) => {
                if !element.sources.contains(&"ocr".to_string()) {
                    element.sources.push("ocr".to_string());
                }
                element.confidence = element.confidence.max(ocr_element.confidence);
            }
            None => elements.push(ocr_element),
        }
    }
    elements
}

fn role_hint_from_control_type(control_type: i32) -> String {
    match control_type {
        50_000 => "button",
        50_004 => "input",
        50_008 => "list",
        50_020 => "text",
        50_032 => "window",
        50_033 => "pane",
        _ => "element",
    }
    .to_string()
}

fn role_hint_from_text(text: &str) -> String {
    if text.contains("发送") || text.contains("搜索") || text.contains("提交") {
        "button"
    } else if text.contains("输入") {
        "input"
    } else if text.contains("表情") || text.contains("文件") || text.contains("红包") {
        "button"
    } else {
        "text"
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(name: &str, x: i32, y: i32, w: i32, h: i32) -> SceneElement {
        SceneElement {
            id: String::new(),
            app_id: "qq".to_string(),
            name: name.to_string(),
            role_hint: "button".to_string(),
            sources: vec!["ocr".to_string()],
            x,
            y,
            width: w,
            height: h,
            confidence: 0.85,
            stale_frames: 0,
            updated_at: "2026-08-13T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn registry_keeps_stable_id_across_small_moves() {
        let mut registry = ElementRegistry::new();
        let frame1 = registry.update("qq", vec![element("发送", 815, 624, 170, 36)]);
        let id1 = frame1[0].id.clone();
        let frame2 = registry.update("qq", vec![element("发送", 820, 630, 170, 36)]);
        assert_eq!(frame2[0].id, id1);
        assert_eq!(frame2[0].stale_frames, 0);
    }

    #[test]
    fn registry_evicts_after_stale_frames() {
        let mut registry = ElementRegistry::new();
        registry.update("qq", vec![element("发送", 815, 624, 170, 36)]);
        for _ in 0..4 {
            registry.update("qq", vec![]);
        }
        assert!(registry.get("qq", "发送").is_none());
    }

    #[test]
    fn registry_assigns_new_ids_to_new_elements() {
        let mut registry = ElementRegistry::new();
        let frame1 = registry.update("qq", vec![element("发送", 815, 624, 170, 36)]);
        let frame2 = registry.update("qq", vec![element("搜索", 100, 100, 50, 20)]);
        let search = frame2
            .iter()
            .find(|entry| entry.name == "搜索")
            .expect("新元素应已注册");
        assert_ne!(frame1[0].id, search.id);
    }

    #[test]
    fn fuse_sources_merges_matching_uia_and_ocr() {
        let uia = vec![UiNode {
            name: "发送".into(),
            control_type: 50_000,
            class: String::new(),
            depth: 1,
            x: 815,
            y: 624,
            width: 170,
            height: 36,
        }];
        let lines = vec![OcrLine {
            text: "发送".into(),
            x: 815,
            y: 624,
            width: 170,
            height: 36,
        }];
        let fused = fuse_sources(&uia, &lines);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].sources, vec!["uia", "ocr"]);
    }
}
