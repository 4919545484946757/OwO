//! 结构化断言与成功定义（v0.5 M-B，对应技术文档 5.8.3）。
//!
//! “输入框清空”等判断改为确定性 `OcrBoxGone{text: "输入消息..."}`（占位符消失），
//! 而不是让视觉模型回答“是否清空”（VL 会把占位符当成未清空）。
//! 静默观察时对“操作后 1–3s 状态 diff”做统计，自动生成默认断言并随技能存储。

use crate::ocr::OcrSummary;
use crate::perception::SituationSnapshot;
use crate::scene::SceneGraph;
use serde::{Deserialize, Serialize};

/// 结构化断言单元：可评估、可学、可存的验证单元。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Assertion {
    WindowTitle {
        expected: String,
    },
    UiaExists {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<String>,
        name: String,
    },
    UiaValue {
        name: String,
        expected: String,
    },
    OcrContains {
        text: String,
    },
    /// 占位符/指定文本的 OCR 框消失（输入框已被真实内容替换或已清空）。
    OcrBoxGone {
        text: String,
    },
    PixelDiff {
        threshold: f64,
    },
    ClipboardChanged {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected: Option<String>,
    },
    VisionConfirm {
        question: String,
    },
    StateDiff {
        entity: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<String>,
    },
}

/// 成功定义：一组断言 + 超时 + 重试。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationRecipe {
    pub assertions: Vec<Assertion>,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_retry")]
    pub retry: u32,
}

fn default_timeout_ms() -> u64 {
    3_000
}

fn default_retry() -> u32 {
    1
}

impl Default for VerificationRecipe {
    fn default() -> Self {
        Self {
            assertions: Vec::new(),
            timeout_ms: default_timeout_ms(),
            retry: default_retry(),
        }
    }
}

impl VerificationRecipe {
    pub fn new(assertions: Vec<Assertion>) -> Self {
        Self {
            assertions,
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.assertions.is_empty()
    }
}

/// 断言的人类可读描述（用于审计与 UI 展示）。
pub fn describe(assertion: &Assertion) -> String {
    match assertion {
        Assertion::WindowTitle { expected } => format!("窗口标题为 {expected}"),
        Assertion::UiaExists { role, name } => match role {
            Some(role) => format!("存在 {role} 元素：{name}"),
            None => format!("存在元素：{name}"),
        },
        Assertion::UiaValue { name, expected } => {
            format!("元素 {name} 值为 {expected}")
        }
        Assertion::OcrContains { text } => format!("OCR 包含文本：{text}"),
        Assertion::OcrBoxGone { text } => format!("OCR 占位框消失：{text}"),
        Assertion::PixelDiff { threshold } => format!("像素差异小于 {threshold}"),
        Assertion::ClipboardChanged { expected } => match expected {
            Some(expected) => format!("剪贴板内容为 {expected}"),
            None => "剪贴板内容已变化".to_string(),
        },
        Assertion::VisionConfirm { question } => format!("视觉确认：{question}"),
        Assertion::StateDiff { entity, from, to } => format!(
            "状态 {entity} 变化：{} -> {}",
            from.as_deref().unwrap_or("?"),
            to.as_deref().unwrap_or("?")
        ),
    }
}

/// 基于情景快照评估断言（仅快照可支撑的子集）。
///
/// 需要 OCR/场景图上下文的断言请使用 [`verify_assertion_full`]；
/// 未接入的断言返回明确错误而非静默通过。
pub fn verify_assertion(
    assertion: &Assertion,
    snapshot: &SituationSnapshot,
) -> Result<bool, String> {
    verify_assertion_full(assertion, snapshot, None, None)
}

/// 基于完整感知上下文评估断言：快照 + OCR 摘要 + 场景图。
///
/// 支持 WindowTitle / UiaExists / OcrContains / OcrBoxGone / ClipboardChanged /
/// StateDiff；PixelDiff、VisionConfirm、UiaValue 当前无法从快照可靠评估，
/// 一律返回明确错误，防止静默通过造成误操作。
pub fn verify_assertion_full(
    assertion: &Assertion,
    snapshot: &SituationSnapshot,
    ocr: Option<&OcrSummary>,
    scene: Option<&SceneGraph>,
) -> Result<bool, String> {
    match assertion {
        Assertion::WindowTitle { expected } => {
            let actual = snapshot
                .foreground_app
                .as_ref()
                .map(|app| app.title.as_str())
                .unwrap_or("");
            Ok(actual == expected || contains_text(actual, expected))
        }
        Assertion::UiaExists { role, name } => verify_uia_exists(snapshot, role, name),
        Assertion::UiaValue { name, expected } => Err(format!(
            "UiaValue 需要元素值字段，当前 UiNode 未提供 value（{name}={expected}）"
        )),
        Assertion::OcrContains { text } => {
            let summary =
                ocr.ok_or_else(|| format!("缺少 OCR 上下文，无法评估：{}", describe(assertion)))?;
            let found = summary
                .boxes
                .iter()
                .any(|box_| contains_text(&box_.text, text))
                || (!summary.text.is_empty() && contains_text(&summary.text, text));
            Ok(found)
        }
        Assertion::OcrBoxGone { text } => {
            let summary =
                ocr.ok_or_else(|| format!("缺少 OCR 上下文，无法评估：{}", describe(assertion)))?;
            Ok(ocr_placeholder_gone(summary, text))
        }
        Assertion::PixelDiff { threshold } => Err(format!(
            "PixelDiff 需要操作前后两帧截图像素差异，当前快照不可评估（threshold={threshold}）"
        )),
        Assertion::ClipboardChanged { expected } => verify_clipboard(snapshot, expected.as_deref()),
        Assertion::VisionConfirm { question } => {
            Err(format!("VisionConfirm 需要调用视觉模型确认：{question}"))
        }
        Assertion::StateDiff { entity, from, to } => {
            verify_state_diff(scene, entity, from.as_deref(), to.as_deref())
        }
    }
}

/// 双向包含匹配（忽略大小写）：名称/占位符等允许部分匹配。
fn contains_text(haystack: &str, needle: &str) -> bool {
    let haystack = haystack.to_lowercase();
    let needle = needle.to_lowercase();
    haystack.contains(&needle) || needle.contains(&haystack)
}

/// OcrBoxGone 的确定性判定：输入框“为空”当且仅当
/// 没有任何 OCR 文本，或全部文本都等于/包含占位符（占位符可见即空框）。
/// 只要存在与占位符不同的真实内容文本，就判定为未清空。
fn ocr_placeholder_gone(summary: &OcrSummary, placeholder: &str) -> bool {
    let texts: Vec<&str> = if summary.boxes.is_empty() {
        if summary.text.trim().is_empty() {
            return true;
        }
        vec![summary.text.as_str()]
    } else {
        summary
            .boxes
            .iter()
            .map(|box_| box_.text.as_str())
            .collect()
    };
    texts.iter().all(|text| contains_text(text, placeholder))
}

fn verify_uia_exists(
    snapshot: &SituationSnapshot,
    role: &Option<String>,
    name: &str,
) -> Result<bool, String> {
    let ui = snapshot
        .ui_context
        .as_ref()
        .ok_or_else(|| "缺少 L1 无障碍 UI 树，无法评估 UiaExists".to_string())?;
    let role_matches = |node: &crate::UiNode| -> bool {
        match role {
            Some(role) => {
                let role = role.to_lowercase();
                node.class.to_lowercase().contains(&role)
                    || control_type_hint(node.control_type)
                        .to_lowercase()
                        .contains(&role)
            }
            None => true,
        }
    };
    Ok(ui
        .ui_tree
        .iter()
        .any(|node| contains_text(&node.name, name) && role_matches(node)))
}

/// 常见 UIA ControlType 数值到角色提示的映射（UIA_ButtonControlTypeId=50000、
/// UIA_EditControlTypeId=50004 等），用于 UiaExists 的 role 过滤。
fn control_type_hint(control_type: i32) -> &'static str {
    match control_type {
        50_000 => "button",
        50_004 => "edit",
        50_020 => "text",
        50_008 => "list",
        50_003 => "combobox",
        50_002 => "checkbox",
        50_015 => "radiobutton",
        50_005 => "hyperlink",
        _ => "",
    }
}

fn verify_clipboard(snapshot: &SituationSnapshot, expected: Option<&str>) -> Result<bool, String> {
    match expected {
        Some(expected) => {
            let content = snapshot
                .content
                .as_ref()
                .ok_or_else(|| "缺少剪贴板内容引用".to_string())?;
            if content.kind != "clipboard" {
                return Err(format!("内容引用类型不是剪贴板：{}", content.kind));
            }
            if content.masked {
                return Err("剪贴板内容已掩码，无法比对预期值".to_string());
            }
            Ok(content
                .snippet
                .as_deref()
                .map(|snippet| contains_text(snippet, expected))
                .unwrap_or(false))
        }
        None => {
            let changed = snapshot
                .recent_actions
                .iter()
                .any(|action| action.contains("剪贴板"))
                || snapshot
                    .content
                    .as_ref()
                    .map(|content| content.kind == "clipboard")
                    .unwrap_or(false);
            if !changed {
                return Err("缺少剪贴板变化证据（recent_actions/content），无法评估".to_string());
            }
            Ok(true)
        }
    }
}

fn verify_state_diff(
    scene: Option<&SceneGraph>,
    entity: &str,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<bool, String> {
    let graph = scene.ok_or_else(|| format!("缺少场景图，无法评估 StateDiff：{entity}"))?;
    let state = graph
        .entity(entity)
        .ok_or_else(|| format!("场景图中不存在实体状态：{entity}"))?;
    if let Some(from) = from {
        if contains_text(&state.value, from) {
            return Ok(false);
        }
    }
    if let Some(to) = to {
        return Ok(contains_text(&state.value, to));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr::OcrBox;
    use crate::perception::{ForegroundApp, UiContext};
    use crate::scene::SceneGraph;

    fn snapshot(title: &str) -> SituationSnapshot {
        SituationSnapshot {
            foreground_app: Some(ForegroundApp {
                id: "qq".to_string(),
                title: title.to_string(),
            }),
            permission_level: "L1".to_string(),
            ui_context: None,
            content: None,
            task_hypothesis: None,
            recent_actions: Vec::new(),
            capture: None,
        }
    }

    fn ocr_summary(texts: &[&str]) -> OcrSummary {
        OcrSummary {
            text: texts.join(" "),
            chars: texts.iter().map(|text| text.chars().count()).sum(),
            boxes: texts
                .iter()
                .map(|text| OcrBox {
                    text: text.to_string(),
                    x: 0,
                    y: 0,
                    width: 100,
                    height: 20,
                })
                .collect(),
            provider: Some("test".to_string()),
        }
    }

    #[test]
    fn window_title_assertion_matches_snapshot() {
        let snapshot = snapshot("QQ - 张三");
        assert!(verify_assertion(
            &Assertion::WindowTitle {
                expected: "QQ".to_string()
            },
            &snapshot
        )
        .unwrap());
        assert!(!verify_assertion(
            &Assertion::WindowTitle {
                expected: "微信".to_string()
            },
            &snapshot
        )
        .unwrap());
    }

    #[test]
    fn unsupported_assertion_returns_error_not_silent_pass() {
        let snapshot = snapshot("QQ");
        let result = verify_assertion(
            &Assertion::OcrBoxGone {
                text: "占位".to_string(),
            },
            &snapshot,
        );
        assert!(result.is_err(), "未接入的断言必须明确报错");
    }

    #[test]
    fn ocr_box_gone_treats_placeholder_as_cleared() {
        let snapshot = snapshot("QQ");
        let ocr = ocr_summary(&["输入消息..."]);
        let result = verify_assertion_full(
            &Assertion::OcrBoxGone {
                text: "输入消息...".to_string(),
            },
            &snapshot,
            Some(&ocr),
            None,
        )
        .unwrap();
        assert!(result, "占位符可见 = 输入框为空 = OcrBoxGone 为真");
    }

    #[test]
    fn ocr_box_gone_false_when_real_content_present() {
        let snapshot = snapshot("QQ");
        let ocr = ocr_summary(&["张三"]);
        let result = verify_assertion_full(
            &Assertion::OcrBoxGone {
                text: "输入消息...".to_string(),
            },
            &snapshot,
            Some(&ocr),
            None,
        )
        .unwrap();
        assert!(!result, "真实内容存在 = 未清空 = OcrBoxGone 为假");
    }

    #[test]
    fn ocr_box_gone_true_when_no_text() {
        let snapshot = snapshot("QQ");
        let ocr = ocr_summary(&[]);
        let result = verify_assertion_full(
            &Assertion::OcrBoxGone {
                text: "输入消息...".to_string(),
            },
            &snapshot,
            Some(&ocr),
            None,
        )
        .unwrap();
        assert!(result, "无任何 OCR 文本 = 已清空");
    }

    #[test]
    fn ocr_contains_matches_boxes() {
        let snapshot = snapshot("QQ");
        let ocr = ocr_summary(&["发送", "文件传输助手"]);
        assert!(verify_assertion_full(
            &Assertion::OcrContains {
                text: "文件".to_string()
            },
            &snapshot,
            Some(&ocr),
            None
        )
        .unwrap());
        assert!(!verify_assertion_full(
            &Assertion::OcrContains {
                text: "不存在".to_string()
            },
            &snapshot,
            Some(&ocr),
            None
        )
        .unwrap());
    }

    #[test]
    fn uia_exists_matches_tree_and_missing_context_errors() {
        let mut snap = snapshot("QQ");
        snap.ui_context = Some(UiContext {
            window: "qq".to_string(),
            active_view: "main".to_string(),
            accessible: true,
            ui_tree: vec![crate::UiNode {
                name: "发送".to_string(),
                control_type: 50_000,
                class: "Button".to_string(),
                depth: 1,
                x: 0,
                y: 0,
                width: 80,
                height: 30,
            }],
        });
        assert!(verify_assertion_full(
            &Assertion::UiaExists {
                role: Some("button".to_string()),
                name: "发送".to_string(),
            },
            &snap,
            None,
            None
        )
        .unwrap());
        let empty = snapshot("QQ");
        let result = verify_assertion_full(
            &Assertion::UiaExists {
                role: None,
                name: "发送".to_string(),
            },
            &empty,
            None,
            None,
        );
        assert!(result.is_err(), "缺少 UI 树必须报错而非静默通过");
    }

    #[test]
    fn state_diff_uses_scene_entities() {
        let snapshot = snapshot("QQ");
        let mut scene = SceneGraph::new();
        scene.add_entity("input_box", "empty", 0.9);
        let result = verify_assertion_full(
            &Assertion::StateDiff {
                entity: "input_box".to_string(),
                from: Some("focused".to_string()),
                to: Some("empty".to_string()),
            },
            &snapshot,
            None,
            Some(&scene),
        )
        .unwrap();
        assert!(result);
        let missing = verify_assertion_full(
            &Assertion::StateDiff {
                entity: "nope".to_string(),
                from: None,
                to: None,
            },
            &snapshot,
            None,
            Some(&scene),
        );
        assert!(missing.is_err(), "不存在的实体必须报错");
    }
}
