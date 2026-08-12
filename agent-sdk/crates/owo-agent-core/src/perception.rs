//! 全域情景感知（v0.4 D19/D22）：L0 事件层 / L1 界面层 / L2 视觉层 / L3 语义层。
//!
//! 隐私边界：
//! - 默认只开 L0/L1；L2 截图按需采集，环形缓冲（内存）不落盘、用后即毁。
//! - 消息/文档内容默认以掩码形式存在，不写入审计与学习样本。
//! - 情景快照由核心统一组装，任何工具/插件不能绕过该接口读取原始感知数据。

use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerceptionLayer {
    L0Event,
    L1Ui,
    L2Visual,
    L3Semantic,
}

impl PerceptionLayer {
    pub fn default_enabled() -> Vec<PerceptionLayer> {
        vec![PerceptionLayer::L0Event, PerceptionLayer::L1Ui]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForegroundApp {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiContext {
    pub window: String,
    pub active_view: String,
    pub accessible: bool,
    /// L1 无障碍 UI 树摘要（语义锚点，按权限过滤）。
    #[serde(default)]
    pub ui_tree: Vec<crate::UiNode>,
}

/// 内容引用：默认 masked=true 且不带 snippet，只有显式会话授权才附最小片段。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentRef {
    pub kind: String,
    pub masked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskHypothesis {
    pub label: String,
    pub confidence: f64,
}

/// L3 语义层 v1：本地启发式任务假设（不调用模型、不上送云端）。
/// 后续可替换为本地小模型推理，接口不变。
pub fn infer_task_hypothesis(app_id: &str, title: &str) -> TaskHypothesis {
    let lower = format!("{app_id} {title}").to_lowercase();
    let (label, confidence) = if ["code", "cursor", "vscode", "visual studio", "jetbrains"]
        .iter()
        .any(|keyword| lower.contains(keyword))
        || lower.contains(".rs")
        || lower.contains(".py")
        || lower.contains(".ts")
    {
        ("coding", 0.8)
    } else if ["qq", "wechat", "weixin", "feishu", "dingtalk"]
        .iter()
        .any(|keyword| lower.contains(keyword))
    {
        ("chatting", 0.8)
    } else if ["steam", "epic", "game", "游戏"]
        .iter()
        .any(|keyword| lower.contains(keyword))
    {
        ("gaming", 0.7)
    } else if ["chrome", "edge", "firefox", "browser"]
        .iter()
        .any(|keyword| lower.contains(keyword))
    {
        ("browsing", 0.7)
    } else {
        ("reading", 0.5)
    };
    TaskHypothesis {
        label: label.to_string(),
        confidence,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureMeta {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// 一次情景快照（Situation Model），结构化、按权限过滤。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SituationSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground_app: Option<ForegroundApp>,
    pub permission_level: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_context: Option<UiContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<ContentRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_hypothesis: Option<TaskHypothesis>,
    #[serde(default)]
    pub recent_actions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<CaptureMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PerceptionEvent {
    ForegroundChanged { app: ForegroundApp },
    UiChanged { ui: UiContext },
    ClipboardMasked { app_id: String },
    Capture { frame: CaptureMeta },
    Hypothesis { hypothesis: TaskHypothesis },
}

impl PerceptionEvent {
    fn layer(&self) -> PerceptionLayer {
        match self {
            PerceptionEvent::ForegroundChanged { .. } | PerceptionEvent::ClipboardMasked { .. } => {
                PerceptionLayer::L0Event
            }
            PerceptionEvent::UiChanged { .. } => PerceptionLayer::L1Ui,
            PerceptionEvent::Capture { .. } => PerceptionLayer::L2Visual,
            PerceptionEvent::Hypothesis { .. } => PerceptionLayer::L3Semantic,
        }
    }

    fn action_label(&self) -> &str {
        match self {
            PerceptionEvent::ForegroundChanged { .. } => "focus",
            PerceptionEvent::UiChanged { .. } => "ui_change",
            PerceptionEvent::ClipboardMasked { .. } => "copy_masked",
            PerceptionEvent::Capture { .. } => "capture",
            PerceptionEvent::Hypothesis { .. } => "hypothesis",
        }
    }
}

pub struct SituationStore {
    enabled: HashSet<PerceptionLayer>,
    foreground: Option<ForegroundApp>,
    ui: Option<UiContext>,
    content: Option<ContentRef>,
    hypothesis: Option<TaskHypothesis>,
    recent_actions: VecDeque<String>,
    /// L2 截图环形缓冲：仅内存，不落盘，用后即毁。
    capture_ring: VecDeque<CaptureFrame>,
    last_clipboard_sequence: u32,
    last_ui_key: String,
    subscribers: Vec<UnboundedSender<PerceptionEvent>>,
    max_ring: usize,
}

struct CaptureFrame {
    meta: CaptureMeta,
    #[allow(dead_code)]
    bytes: Vec<u8>,
}

impl Default for SituationStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SituationStore {
    pub fn new() -> Self {
        Self {
            enabled: PerceptionLayer::default_enabled().into_iter().collect(),
            foreground: None,
            ui: None,
            content: None,
            hypothesis: None,
            recent_actions: VecDeque::new(),
            capture_ring: VecDeque::new(),
            last_clipboard_sequence: 0,
            last_ui_key: String::new(),
            subscribers: Vec::new(),
            max_ring: 5,
        }
    }

    pub fn set_layer_enabled(&mut self, layer: PerceptionLayer, enabled: bool) {
        if enabled {
            self.enabled.insert(layer);
        } else {
            self.enabled.remove(&layer);
        }
    }

    pub fn is_enabled(&self, layer: PerceptionLayer) -> bool {
        self.enabled.contains(&layer)
    }

    pub fn subscribe(&mut self) -> UnboundedReceiver<PerceptionEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.subscribers.push(tx);
        rx
    }

    /// 记录事件：层未授权直接拒绝（deny 优先）。
    pub fn record_event(&mut self, event: PerceptionEvent) -> Result<(), String> {
        if !self.enabled.contains(&event.layer()) {
            return Err(format!("感知层未授权：{:?}", event.layer()));
        }
        let label = event.action_label().to_string();
        match &event {
            PerceptionEvent::ForegroundChanged { app } => self.foreground = Some(app.clone()),
            PerceptionEvent::UiChanged { ui } => self.ui = Some(ui.clone()),
            PerceptionEvent::ClipboardMasked { .. } => {}
            PerceptionEvent::Capture { frame } => {
                if self.capture_ring.len() >= self.max_ring {
                    self.capture_ring.pop_front();
                }
                self.capture_ring.push_back(CaptureFrame {
                    meta: frame.clone(),
                    bytes: Vec::new(),
                });
            }
            PerceptionEvent::Hypothesis { hypothesis } => {
                self.hypothesis = Some(hypothesis.clone())
            }
        }
        self.recent_actions.push_back(label);
        if self.recent_actions.len() > 20 {
            self.recent_actions.pop_front();
        }
        self.subscribers
            .retain(|sender| sender.send(event.clone()).is_ok());
        Ok(())
    }

    /// 会话级内容授权：聊天/文档内容仅在显式授权后附带最小片段。
    pub fn authorize_content(&mut self, kind: &str, snippet: Option<String>) {
        self.content = Some(ContentRef {
            kind: kind.to_string(),
            masked: snippet.is_none(),
            snippet,
        });
    }

    /// L2 按需采集：生成帧元数据进环形缓冲（不落盘）。
    pub fn begin_capture(&mut self, summary: String) -> Result<CaptureMeta, String> {
        if !self.is_enabled(PerceptionLayer::L2Visual) {
            return Err("L2 视觉层未授权".to_string());
        }
        let frame = CaptureMeta {
            id: uuid::Uuid::new_v4().to_string(),
            captured_at: Some(chrono::Utc::now().to_rfc3339()),
            summary: Some(summary),
        };
        self.record_event(PerceptionEvent::Capture {
            frame: frame.clone(),
        })?;
        Ok(frame)
    }

    /// L2 按需采集：抓取真实屏幕到内存环形缓冲（不落盘、用后即毁）。
    pub fn begin_capture_from_screen(&mut self) -> Result<CaptureMeta, String> {
        self.begin_capture_bytes(crate::platform::capture_screen().ok_or("屏幕截图失败")?)
    }

    /// L2 按需采集：抓取指定区域（测试/预览用）。
    pub fn begin_capture_region(&mut self, width: i32, height: i32) -> Result<CaptureMeta, String> {
        self.begin_capture_bytes(
            crate::platform::capture_screen_region(width, height).ok_or("屏幕截图失败")?,
        )
    }

    fn begin_capture_bytes(&mut self, bytes: Vec<u8>) -> Result<CaptureMeta, String> {
        if !self.is_enabled(PerceptionLayer::L2Visual) {
            return Err("L2 视觉层未授权".to_string());
        }
        let mut summary = format!("内存截图 {} bytes", bytes.len());
        if let Some(ocr) = crate::ocr::ocr_bmp(&bytes) {
            let mut text = ocr.text.trim().to_string();
            if text.chars().count() > 200 {
                text = text.chars().take(200).collect();
                text.push('…');
            }
            summary.push_str(&format!(" | OCR: {text}"));
        }
        let frame = CaptureMeta {
            id: uuid::Uuid::new_v4().to_string(),
            captured_at: Some(chrono::Utc::now().to_rfc3339()),
            summary: Some(summary),
        };
        if self.capture_ring.len() >= self.max_ring {
            self.capture_ring.pop_front();
        }
        self.capture_ring.push_back(CaptureFrame {
            meta: frame.clone(),
            bytes,
        });
        self.recent_actions.push_back("capture".to_string());
        if self.recent_actions.len() > 20 {
            self.recent_actions.pop_front();
        }
        self.notify(PerceptionEvent::Capture {
            frame: frame.clone(),
        });
        Ok(frame)
    }

    /// L0 剪贴板事件源：只记录“内容已变化”（掩码），不读取内容。
    pub fn refresh_clipboard(&mut self, sequence: u32) {
        if sequence == 0 || self.last_clipboard_sequence == sequence {
            return;
        }
        self.last_clipboard_sequence = sequence;
        let app_id = self
            .foreground
            .as_ref()
            .map(|app| app.id.clone())
            .unwrap_or_else(|| "unknown".to_string());
        let _ = self.record_event(PerceptionEvent::ClipboardMasked { app_id });
    }

    /// L1 界面层：抓取前台窗口无障碍 UI 树摘要；内容未变化时不重复记录事件。
    pub fn refresh_from_uia(&mut self, max_depth: u32, max_nodes: usize) -> Result<usize, String> {
        if !self.is_enabled(PerceptionLayer::L1Ui) {
            return Err("L1 界面层未授权".to_string());
        }
        let tree = crate::accessibility::foreground_ui_tree(max_depth, max_nodes)
            .ok_or("UIA 不可用或无前台窗口")?;
        let key = tree
            .iter()
            .map(|node| format!("{}|{}|{}", node.name, node.control_type, node.class))
            .collect::<Vec<_>>()
            .join("\n");
        let window = self
            .foreground
            .as_ref()
            .map(|app| app.title.clone())
            .unwrap_or_default();
        let active_view = tree
            .first()
            .map(|node| node.name.clone())
            .unwrap_or_default();
        let ui = UiContext {
            window,
            active_view,
            accessible: true,
            ui_tree: tree,
        };
        if self.last_ui_key != key {
            self.last_ui_key = key;
            let _ = self.record_event(PerceptionEvent::UiChanged { ui: ui.clone() });
        }
        Ok(ui.ui_tree.len())
    }

    fn notify(&mut self, event: PerceptionEvent) {
        self.subscribers
            .retain(|sender| sender.send(event.clone()).is_ok());
    }

    /// 任务结束立即销毁截图缓冲。
    pub fn discard_captures(&mut self) {
        self.capture_ring.clear();
    }

    pub fn set_task_hypothesis(&mut self, label: &str, confidence: f64) {
        let hypothesis = TaskHypothesis {
            label: label.to_string(),
            confidence,
        };
        let _ = self.record_event(PerceptionEvent::Hypothesis { hypothesis });
    }

    /// L3 语义层：按当前前台应用推断任务假设（仅 L3 授权时生效，变化才记录）。
    pub fn refresh_task_hypothesis(&mut self) {
        if !self.is_enabled(PerceptionLayer::L3Semantic) {
            return;
        }
        let Some(app) = &self.foreground else {
            return;
        };
        let hypothesis = infer_task_hypothesis(&app.id, &app.title);
        let changed = self
            .hypothesis
            .as_ref()
            .map(|current| current.label != hypothesis.label)
            .unwrap_or(true);
        if changed {
            self.set_task_hypothesis(&hypothesis.label, hypothesis.confidence);
        }
    }

    /// L0 事件源：从平台轮询前台应用并记录（Windows 前台窗口）。
    /// 前台应用无变化时返回当前缓存，不重复记录事件。
    pub fn refresh_from_platform(&mut self) -> Option<ForegroundApp> {
        #[cfg(target_os = "windows")]
        {
            let (id, title) = crate::platform::poll_foreground_app()?;
            let unchanged = self
                .foreground
                .as_ref()
                .map(|app| app.id == id && app.title == title)
                .unwrap_or(false);
            let app = ForegroundApp { id, title };
            if !unchanged {
                let _ = self.record_event(PerceptionEvent::ForegroundChanged { app: app.clone() });
            }
            self.refresh_task_hypothesis();
            Some(app)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = &self;
            None
        }
    }

    pub fn clear(&mut self) {
        self.recent_actions.clear();
        self.capture_ring.clear();
    }

    pub fn capture_ring_len(&self) -> usize {
        self.capture_ring.len()
    }

    pub fn recent_actions(&self) -> Vec<String> {
        self.recent_actions.iter().cloned().collect()
    }

    /// 组装当前情景快照（按权限过滤；内容默认掩码）。
    pub fn snapshot(&self) -> SituationSnapshot {
        let mut permission_level = "l0_l1".to_string();
        if self.is_enabled(PerceptionLayer::L2Visual) {
            permission_level = "l2_visual".to_string();
        }
        if self.is_enabled(PerceptionLayer::L3Semantic) {
            permission_level = "l3_semantic".to_string();
        }
        SituationSnapshot {
            foreground_app: self.foreground.clone(),
            permission_level,
            ui_context: self.ui.clone(),
            content: self.content.clone(),
            task_hypothesis: self.hypothesis.clone(),
            recent_actions: self.recent_actions(),
            capture: self
                .capture_ring
                .back()
                .map(|frame| frame.meta.clone())
                .filter(|_| self.is_enabled(PerceptionLayer::L2Visual)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> ForegroundApp {
        ForegroundApp {
            id: "code".to_string(),
            title: "VSCode - parser.rs".to_string(),
        }
    }

    #[test]
    fn default_layers_l0_l1_and_content_masked_by_default() {
        let mut store = SituationStore::new();
        store
            .record_event(PerceptionEvent::ForegroundChanged { app: app() })
            .unwrap();
        store.authorize_content("conversation", None);
        let snapshot = store.snapshot();
        assert_eq!(snapshot.foreground_app.as_ref().unwrap().id, "code");
        assert_eq!(snapshot.permission_level, "l0_l1");
        assert!(snapshot.content.as_ref().unwrap().masked);
        assert!(snapshot.content.as_ref().unwrap().snippet.is_none());
        assert!(snapshot.capture.is_none());
    }

    #[test]
    fn l2_capture_is_ring_buffered_and_discarded() {
        let mut store = SituationStore::new();
        store.set_layer_enabled(PerceptionLayer::L2Visual, true);
        for index in 0..7 {
            store
                .begin_capture(format!("frame-{index}"))
                .expect("L2 enabled");
        }
        assert_eq!(store.capture_ring_len(), 5);
        let snapshot = store.snapshot();
        assert_eq!(snapshot.permission_level, "l2_visual");
        assert!(snapshot.capture.is_some());
        store.discard_captures();
        assert_eq!(store.capture_ring_len(), 0);
        assert!(store.snapshot().capture.is_none());
    }

    #[test]
    fn disabled_layer_rejects_events() {
        let mut store = SituationStore::new();
        store.set_layer_enabled(PerceptionLayer::L3Semantic, false);
        assert!(store
            .record_event(PerceptionEvent::Hypothesis {
                hypothesis: TaskHypothesis {
                    label: "coding".to_string(),
                    confidence: 0.9,
                }
            })
            .is_err());
        assert!(store.snapshot().task_hypothesis.is_none());
    }

    #[test]
    fn subscribe_receives_events() {
        let mut store = SituationStore::new();
        let mut rx = store.subscribe();
        store
            .record_event(PerceptionEvent::ForegroundChanged { app: app() })
            .unwrap();
        let received = rx.try_recv().unwrap();
        match received {
            PerceptionEvent::ForegroundChanged { app } => assert_eq!(app.id, "code"),
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn hypothesis_updates_snapshot_when_enabled() {
        let mut store = SituationStore::new();
        store.set_layer_enabled(PerceptionLayer::L3Semantic, true);
        store.set_task_hypothesis("coding", 0.85);
        let snapshot = store.snapshot();
        assert_eq!(snapshot.permission_level, "l3_semantic");
        assert_eq!(snapshot.task_hypothesis.unwrap().label, "coding");
        assert!(snapshot.recent_actions.contains(&"hypothesis".to_string()));
    }

    #[test]
    fn platform_refresh_is_callable() {
        let mut store = SituationStore::new();
        let _ = store.refresh_from_platform();
        assert!(store.snapshot().permission_level == "l0_l1");
    }

    #[test]
    fn clipboard_change_records_masked_event_once() {
        let mut store = SituationStore::new();
        store.refresh_clipboard(1);
        store.refresh_clipboard(1); // 未变化不重复
        store.refresh_clipboard(2);
        let snapshot = store.snapshot();
        assert_eq!(
            snapshot
                .recent_actions
                .iter()
                .filter(|action| action.as_str() == "copy_masked")
                .count(),
            2
        );
    }

    #[test]
    fn screen_capture_enters_ring_and_is_discarded() {
        let mut store = SituationStore::new();
        store.set_layer_enabled(PerceptionLayer::L2Visual, true);
        match store.begin_capture_from_screen() {
            Ok(meta) => {
                assert!(meta.summary.unwrap().contains("内存截图"));
                assert_eq!(store.capture_ring_len(), 1);
                store.discard_captures();
                assert_eq!(store.capture_ring_len(), 0);
            }
            Err(_) => {
                // 无窗口会话允许截图失败（隐私优先：不强制采集）。
            }
        }
    }

    #[test]
    fn ui_tree_refresh_is_callable() {
        let mut store = SituationStore::new();
        match store.refresh_from_uia(2, 32) {
            Ok(count) => {
                assert!(count > 0);
                assert!(!store.snapshot().ui_context.unwrap().ui_tree.is_empty());
            }
            Err(_) => {
                // 无前台窗口/UIA 不可用时允许失败（不强制采集）。
            }
        }
    }

    #[test]
    fn infers_task_hypothesis_by_app() {
        let coding = infer_task_hypothesis("code", "parser.rs - VSCode");
        assert_eq!(coding.label, "coding");
        assert!(coding.confidence >= 0.8);
        let chat = infer_task_hypothesis("qq", "QQ - 项目群");
        assert_eq!(chat.label, "chatting");
        let game = infer_task_hypothesis("some-game", "Game Window");
        assert_eq!(game.label, "gaming");
        let browse = infer_task_hypothesis("chrome", "知乎 - Google Chrome");
        assert_eq!(browse.label, "browsing");
    }

    #[test]
    fn l3_hypothesis_updates_only_when_changed() {
        let mut store = SituationStore::new();
        store.set_layer_enabled(PerceptionLayer::L3Semantic, true);
        store
            .record_event(PerceptionEvent::ForegroundChanged {
                app: ForegroundApp {
                    id: "code".to_string(),
                    title: "main.rs - VSCode".to_string(),
                },
            })
            .unwrap();
        store.refresh_task_hypothesis();
        let snapshot = store.snapshot();
        assert_eq!(snapshot.task_hypothesis.unwrap().label, "coding");
        let actions_after_first = store.recent_actions().len();
        store.refresh_task_hypothesis(); // 未变化不重复记录
        assert_eq!(store.recent_actions().len(), actions_after_first);
    }
}
