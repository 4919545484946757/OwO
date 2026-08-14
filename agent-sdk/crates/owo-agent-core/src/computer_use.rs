//! 桌面操作与浏览器自动化工具（v0.4.1 计算机使用）。
//!
//! 桌面工具走“OCR 定位 → SendInput 点击/输入 → OCR 验证”的确定性控制链路；
//! 浏览器工具走 Playwright（本机 Edge + 持久化 profile），文件类能力优先后端完成。
//!
//! 浏览器驱动为 Node 子进程（stdin/stdout JSONL 协议），脚本内嵌于编译产物，
//! 首次调用时写出到临时目录并保持常驻，页面状态跨工具调用不丢失。

use crate::executor;
use crate::tools::{required_string, resolve_session_path, ToolContext, ToolSpec};
use crate::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex as AsyncMutex;

// ---------- 桌面工具 ----------

/// 模拟环境开关：设置 OWO_SIM_QQ_URL（如 http://127.0.0.1:18500）后，
/// 桌面工具全部落到虚拟窗口（离屏渲染 + HTTP 输入），不触碰真实桌面。
fn sim_base_url() -> Option<String> {
    std::env::var("OWO_SIM_QQ_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 是否配置了模拟面（供服务端接线选择 SimTaskSurface / RealTaskSurface）。
pub fn sim_base_url_configured() -> bool {
    sim_base_url().is_some()
}

fn on_sim_surface() -> bool {
    sim_base_url().is_some()
}

async fn sim_fetch_frame() -> Result<Vec<u8>, String> {
    let base = sim_base_url().ok_or("模拟环境未配置 OWO_SIM_QQ_URL")?;
    let url = format!("{}/frame", base.trim_end_matches('/'));
    let response = reqwest::get(&url)
        .await
        .map_err(|e| format!("模拟窗口截图失败：{e}"))?;
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("模拟窗口截图读取失败：{e}"))?
        .to_vec();
    if bytes.len() < 54 || &bytes[..2] != b"BM" {
        return Err("模拟窗口返回的不是 BMP".to_string());
    }
    Ok(bytes)
}

/// 模拟面真值版面（优先）：模拟服务知道每个控件的文字与位置，直接返回
/// 与 screen_ocr 同构的 lines，避免离屏渲染 + Media.Ocr 的小字识别问题。
async fn sim_ocr_lines() -> Option<Value> {
    let base = sim_base_url()?;
    let url = format!("{}/ocr", base.trim_end_matches('/'));
    let response = reqwest::get(&url).await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let value: Value = response.json().await.ok()?;
    let has_lines = value
        .get("lines")
        .and_then(Value::as_array)
        .map(|lines| !lines.is_empty())
        .unwrap_or(false);
    if !has_lines {
        return None;
    }
    Some(value)
}

/// 向指定模拟服务地址 POST JSON（同 `sim_post`，但 base 由调用方给定）。
async fn sim_post_at(base: &str, path: &str, body: Value) -> Result<Value, String> {
    let url = format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    let client = reqwest::Client::new();
    let response = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("模拟窗口 {path} 失败：{e}"))?;
    response
        .json::<Value>()
        .await
        .map_err(|e| format!("模拟窗口 {path} 响应解析失败：{e}"))
}

async fn sim_post(path: &str, body: Value) -> Result<Value, String> {
    let base = sim_base_url().ok_or("模拟环境未配置 OWO_SIM_QQ_URL")?;
    let url = format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    let client = reqwest::Client::new();
    let response = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("模拟窗口 {path} 失败：{e}"))?;
    response
        .json::<Value>()
        .await
        .map_err(|e| format!("模拟窗口 {path} 响应解析失败：{e}"))
}

fn ocr_summary_json(summary: &crate::ocr::OcrSummary, max_boxes: usize) -> Value {
    // 控制传给模型的上下文体积：超大 OCR 结果会让多轮工具调用不稳定。
    let max_lines = 60usize;
    let max_boxes = max_boxes.min(80);
    let text: String = summary.text.chars().take(2000).collect();
    let lines: Vec<Value> = crate::ocr::group_ocr_lines(&summary.boxes)
        .into_iter()
        .take(max_lines)
        .map(|line| {
            let role_hint = if line.text.contains("发送")
                || line.text.contains("搜索")
                || line.text.contains("提交")
            {
                "button"
            } else if line.text.contains("输入")
                || line.text.contains("搜索")
                || line.text.contains("请输入")
            {
                "input"
            } else if line.y < 60 {
                "header"
            } else {
                "text"
            };
            json!({
                "text": line.text,
                "x": line.x,
                "y": line.y,
                "width": line.width,
                "height": line.height,
                "role_hint": role_hint,
            })
        })
        .collect();
    let boxes: Vec<Value> = summary
        .boxes
        .iter()
        .take(max_boxes)
        .map(|b| {
            json!({
                "text": b.text,
                "x": b.x,
                "y": b.y,
                "width": b.width,
                "height": b.height,
            })
        })
        .collect();
    json!({
        "text": text,
        "chars": summary.chars,
        "lines": lines,
        "boxes": boxes,
        "box_count": summary.boxes.len(),
    })
}

/// 统一 OCR 入口（模拟面走真值版面，真实面走 Media.Ocr），返回 screen_ocr 同构 JSON。
async fn ocr_screen(max_boxes: usize) -> Result<Value, String> {
    if on_sim_surface() {
        if let Some(mut result) = sim_ocr_lines().await {
            if let Value::Object(map) = &mut result {
                map.insert("surface".into(), json!("sim"));
            }
            return Ok(result);
        }
    }
    let (bmp, surface) = if on_sim_surface() {
        (sim_fetch_frame().await?, "sim")
    } else {
        (
            crate::platform::capture_screen().ok_or("屏幕截图失败")?,
            "desktop",
        )
    };
    let summary = crate::paddle_ocr::ocr_preferred(&bmp)
        .await
        .map_err(|e| format!("OCR 失败：{e}"))?;
    let mut result = ocr_summary_json(&summary, max_boxes);
    if let Value::Object(map) = &mut result {
        map.insert("surface".into(), json!(surface));
    }
    Ok(result)
}

/// 在 OCR lines 中查找包含目标文本的行（可带 role_hint 过滤）。
fn find_ocr_line(ocr: &Value, needle: &str, role: &str) -> Option<Value> {
    let needle_lower = needle.to_lowercase();
    let lines = ocr.get("lines")?.as_array()?;
    lines
        .iter()
        .find(|line| {
            let text = line
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_lowercase();
            let role_ok = role.is_empty()
                || line
                    .get("role_hint")
                    .and_then(Value::as_str)
                    .map(|line_role| line_role == role)
                    .unwrap_or(false);
            role_ok && text.contains(&needle_lower)
        })
        .cloned()
}

/// 公开同步入口：屏幕坐标单击（HTTP 服务与 Agent 工具共用）。
pub fn desktop_click(x: i32, y: i32) -> Result<(), String> {
    executor::click_at_screen(x, y)
}

/// 公开同步入口：注入 UTF-16 文本。
pub fn desktop_type(text: &str) -> Result<(), String> {
    executor::send_unicode(text)
}

/// 公开同步入口：发送单个按键（enter/tab 等）。
pub fn desktop_key(key: &str) -> Result<(), String> {
    executor::send_shortcut(key)
}

/// 公开同步入口：发送组合键。
pub fn desktop_shortcut(combo: &str) -> Result<(), String> {
    executor::send_shortcut(combo)
}

/// 公开同步入口：启动应用/URL。
pub fn desktop_launch(target: &str) -> Result<(), String> {
    executor::launch_target(target)
}

/// 公开同步入口：屏幕坐标处滚轮（正上负下）。
pub fn desktop_scroll(x: i32, y: i32, delta: i32) -> Result<(), String> {
    executor::scroll_at_screen(x, y, delta)
}

// ---------- m4d 任务级门禁与审批闭环 ----------

/// 任务动作门禁：任何 desktop_* 动作执行前必须通过本校验（m4d 审批版）。
///
/// 校验顺序：任务存在 → 状态可执行（含超时）→ 动作在允许集 → 目标应用匹配 →
/// 敏感 UI 熔断 → 动作预算。任一失败返回 Err（**不执行动作**），并落审计
/// `permission/deny`（approved=false）；全部通过时记审计 `permission/grant`。
///
/// `sensitive_sample` 为当前界面的敏感检测样本 `(name, role, ocr_text)`；
/// 命中密码/支付/验证码等关键词时任务置 Fused 并要求人工接管。
pub fn task_gate_check(
    registry: &crate::computer_task::ComputerTaskRegistry,
    mut audit: Option<&mut crate::audit::AuditLog>,
    session_id: &str,
    task_id: &str,
    action: &str,
    actual_app: &str,
    sensitive_sample: Option<(&str, &str, &str)>,
) -> Result<(), String> {
    let deny = |audit: &mut crate::audit::AuditLog, detail: String| {
        audit.record(
            session_id,
            "permission",
            Some(action.to_string()),
            Some(false),
            detail,
        );
    };
    // 1. 任务存在 + 状态可执行（含超时自动暂停）。
    if let Err(e) = registry.check_can_execute(task_id) {
        if let Some(a) = audit.as_mut() {
            deny(a, format!("computer-use 门禁拒绝：{e}"));
        }
        return Err(e);
    }
    // 2. 动作允许集。
    if let Err(e) = registry.check_action_allowed(task_id, action) {
        if let Some(a) = audit.as_mut() {
            deny(a, format!("computer-use 门禁拒绝：{e}"));
        }
        return Err(e);
    }
    // 3. 目标应用匹配。
    match registry.target_matches(task_id, actual_app) {
        Ok(true) => {}
        Ok(false) => {
            let detail = format!(
                "computer-use 门禁拒绝：任务 {task_id} 目标应用不匹配（任务声明与当前 {actual_app}）"
            );
            if let Some(a) = audit.as_mut() {
                deny(a, detail);
            }
            return Err(format!(
                "目标应用 {actual_app} 不在任务 {task_id} 授权范围内"
            ));
        }
        Err(e) => {
            if let Some(a) = audit.as_mut() {
                deny(a, format!("computer-use 门禁拒绝：{e}"));
            }
            return Err(e);
        }
    }
    // 4. 敏感 UI 熔断。
    if let Some((name, role, ocr_text)) = sensitive_sample {
        if let Some(reason) = crate::computer_task::sensitive_ui_hit(name, role, ocr_text) {
            let detail = format!("computer-use 敏感熔断：{reason}，任务 {task_id} 置 Fused");
            let _ = registry.fuse(task_id, &detail);
            if let Some(a) = audit.as_mut() {
                deny(a, detail);
            }
            return Err(format!(
                "敏感 UI 熔断：{reason}；任务 {task_id} 已暂停，需人工接管后 resume"
            ));
        }
    }
    // 5. 动作预算。
    if let Err(e) = registry.check_action_budget(task_id) {
        if let Some(a) = audit.as_mut() {
            deny(a, format!("computer-use 门禁拒绝：{e}"));
        }
        return Err(e);
    }
    if let Some(a) = audit.as_mut() {
        a.record(
            session_id,
            "permission",
            Some(action.to_string()),
            Some(true),
            format!("computer-use 任务 {task_id} 动作 {action} 已获授权（目标 {actual_app}）"),
        );
    }
    Ok(())
}

/// 对 OCR lines 做整屏敏感扫描，返回首个命中说明（无命中返回 None）。
pub fn scan_ui_sensitive(ocr: &Value) -> Option<String> {
    let lines = ocr.get("lines")?.as_array()?;
    for line in lines {
        let text = line.get("text").and_then(Value::as_str).unwrap_or("");
        let role = line.get("role_hint").and_then(Value::as_str).unwrap_or("");
        if let Some(reason) = crate::computer_task::sensitive_ui_hit(text, role, "") {
            return Some(reason);
        }
    }
    None
}

/// 门禁后屏幕单击（sim/真实统一走各自实现）。
#[allow(clippy::too_many_arguments)]
pub async fn desktop_click_gated(
    registry: &crate::computer_task::ComputerTaskRegistry,
    mut audit: Option<&mut crate::audit::AuditLog>,
    session_id: &str,
    task_id: &str,
    app: &str,
    x: i32,
    y: i32,
    sensitive_sample: Option<(&str, &str, &str)>,
) -> Result<Value, String> {
    task_gate_check(
        registry,
        audit.as_deref_mut(),
        session_id,
        task_id,
        "desktop_click",
        app,
        sensitive_sample,
    )?;
    let result = if on_sim_surface() {
        sim_post("click", json!({ "x": x, "y": y })).await?
    } else {
        executor::click_at_screen(x, y)?;
        json!({ "clicked": [x, y] })
    };
    registry.record_action(task_id);
    if let Some(a) = audit.as_mut() {
        a.record(
            session_id,
            "tool_call",
            Some("desktop_click".to_string()),
            Some(true),
            format!("任务 {task_id} 点击 ({x},{y})"),
        );
    }
    Ok(result)
}

/// 门禁后注入文本。
#[allow(clippy::too_many_arguments)]
pub async fn desktop_type_gated(
    registry: &crate::computer_task::ComputerTaskRegistry,
    mut audit: Option<&mut crate::audit::AuditLog>,
    session_id: &str,
    task_id: &str,
    app: &str,
    text: &str,
    sensitive_sample: Option<(&str, &str, &str)>,
) -> Result<Value, String> {
    task_gate_check(
        registry,
        audit.as_deref_mut(),
        session_id,
        task_id,
        "desktop_type",
        app,
        sensitive_sample,
    )?;
    let result = if on_sim_surface() {
        sim_post("type", json!({ "text": text })).await?
    } else {
        executor::send_unicode(text)?;
        json!({ "typed_chars": text.chars().count() })
    };
    registry.record_action(task_id);
    if let Some(a) = audit.as_mut() {
        a.record(
            session_id,
            "tool_call",
            Some("desktop_type".to_string()),
            Some(true),
            format!("任务 {task_id} 输入 {} 字符", text.chars().count()),
        );
    }
    Ok(result)
}

/// 门禁后发送按键（enter/tab/backspace 等）。
#[allow(clippy::too_many_arguments)]
pub async fn desktop_key_gated(
    registry: &crate::computer_task::ComputerTaskRegistry,
    mut audit: Option<&mut crate::audit::AuditLog>,
    session_id: &str,
    task_id: &str,
    app: &str,
    key: &str,
    sensitive_sample: Option<(&str, &str, &str)>,
) -> Result<Value, String> {
    task_gate_check(
        registry,
        audit.as_deref_mut(),
        session_id,
        task_id,
        "desktop_key",
        app,
        sensitive_sample,
    )?;
    let result = if on_sim_surface() {
        sim_post("key", json!({ "key": key })).await?
    } else {
        executor::send_shortcut(key)?;
        json!({ "key": key })
    };
    registry.record_action(task_id);
    if let Some(a) = audit.as_mut() {
        a.record(
            session_id,
            "tool_call",
            Some("desktop_key".to_string()),
            Some(true),
            format!("任务 {task_id} 按键 {key}"),
        );
    }
    Ok(result)
}

/// 门禁后滚轮。
#[allow(clippy::too_many_arguments)]
pub async fn desktop_scroll_gated(
    registry: &crate::computer_task::ComputerTaskRegistry,
    mut audit: Option<&mut crate::audit::AuditLog>,
    session_id: &str,
    task_id: &str,
    app: &str,
    x: i32,
    y: i32,
    delta: i32,
    sensitive_sample: Option<(&str, &str, &str)>,
) -> Result<Value, String> {
    task_gate_check(
        registry,
        audit.as_deref_mut(),
        session_id,
        task_id,
        "desktop_scroll",
        app,
        sensitive_sample,
    )?;
    let result = if on_sim_surface() {
        sim_post("scroll", json!({ "x": x, "y": y, "delta": delta })).await?
    } else {
        executor::scroll_at_screen(x, y, delta)?;
        json!({ "scrolled": [x, y, delta] })
    };
    registry.record_action(task_id);
    if let Some(a) = audit.as_mut() {
        a.record(
            session_id,
            "tool_call",
            Some("desktop_scroll".to_string()),
            Some(true),
            format!("任务 {task_id} 滚轮 ({x},{y},{delta})"),
        );
    }
    Ok(result)
}

/// 门禁后启动应用/URL（任务已批准且目标应用匹配时才允许）。
#[allow(clippy::too_many_arguments)]
pub async fn desktop_launch_gated(
    registry: &crate::computer_task::ComputerTaskRegistry,
    mut audit: Option<&mut crate::audit::AuditLog>,
    session_id: &str,
    task_id: &str,
    app: &str,
    target: &str,
) -> Result<Value, String> {
    task_gate_check(
        registry,
        audit.as_deref_mut(),
        session_id,
        task_id,
        "desktop_launch",
        app,
        None,
    )?;
    let result = if on_sim_surface() {
        json!({ "launched": target, "surface": "sim" })
    } else {
        executor::launch_target(target)?;
        json!({ "launched": target })
    };
    registry.record_action(task_id);
    if let Some(a) = audit.as_mut() {
        a.record(
            session_id,
            "tool_call",
            Some("desktop_launch".to_string()),
            Some(true),
            format!("任务 {task_id} 启动 {target}"),
        );
    }
    Ok(result)
}

/// 闭环单步目标：感知到 `anchor_text` 后执行动作，并用 `verify_text` 验证。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TaskGoal {
    /// 定位锚点（OCR 文本，大小写不敏感子串匹配）。
    pub anchor_text: String,
    /// 动作类型：click / type / key。
    pub action: String,
    /// 动作载荷：type 的文本或 key 名（click 忽略）。
    pub value: String,
    /// 动作后的验证文本（可选；出现在下一轮 OCR 即视为验证通过）。
    pub verify_text: Option<String>,
}

/// 闭环执行报告。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TaskReport {
    pub task_id: String,
    pub steps: usize,
    pub state: crate::computer_task::TaskState,
    pub detail: String,
}

/// 感知闭环执行所需的桌面面抽象：感知（OCR 版面）与动作注入。
///
/// 运行环境用 [`SimTaskSurface`]（owo-sim-qq）；契约测试注入内存 Mock，
/// 使闭环在无网络/无真实桌面时完整可测。
#[async_trait::async_trait]
pub trait TaskSurface: Send {
    /// 当前前台应用标识（用于目标应用匹配）。
    fn app(&self) -> String;
    /// 当前 OCR 版面（screen_ocr 同构：lines 数组，每行 text/x/y/width/height/role_hint）。
    async fn ocr(&mut self) -> Result<Value, String>;
    async fn click(&mut self, x: i32, y: i32) -> Result<(), String>;
    async fn type_text(&mut self, text: &str) -> Result<(), String>;
    async fn key(&mut self, key: &str) -> Result<(), String>;
    async fn launch(&mut self, target: &str) -> Result<(), String>;
}

/// owo-sim-qq HTTP 模拟面（`OWO_SIM_QQ_URL` 指向模拟窗口）。
#[derive(Debug)]
pub struct SimTaskSurface {
    base: String,
}

impl SimTaskSurface {
    pub fn new() -> Result<Self, String> {
        let base = sim_base_url().ok_or("模拟环境未配置 OWO_SIM_QQ_URL")?;
        Ok(Self { base })
    }
}

/// 真实桌面面：OCR 走本地引擎（Media.Ocr / PP-OCRv6 / 本地 ONNX），
/// 动作走 executor（SendInput / UIA / 启动）。用于已授权任务在真实桌面的闭环。
#[derive(Debug, Default)]
pub struct RealTaskSurface;

#[async_trait::async_trait]
impl TaskSurface for RealTaskSurface {
    fn app(&self) -> String {
        crate::platform::poll_foreground_app()
            .map(|(app_id, _)| app_id)
            .unwrap_or_default()
    }

    async fn ocr(&mut self) -> Result<Value, String> {
        ocr_screen(0).await
    }

    async fn click(&mut self, x: i32, y: i32) -> Result<(), String> {
        executor::click_at_screen(x, y)
    }

    async fn type_text(&mut self, text: &str) -> Result<(), String> {
        executor::send_unicode(text)
    }

    async fn key(&mut self, key: &str) -> Result<(), String> {
        executor::send_shortcut(key)
    }

    async fn launch(&mut self, target: &str) -> Result<(), String> {
        executor::launch_target(target)
    }
}

#[async_trait::async_trait]
impl TaskSurface for SimTaskSurface {
    fn app(&self) -> String {
        "owo-sim-qq".to_string()
    }

    async fn ocr(&mut self) -> Result<Value, String> {
        let base = self.base.trim_end_matches('/');
        let response = reqwest::get(format!("{base}/ocr"))
            .await
            .map_err(|e| format!("模拟面 OCR 失败：{e}"))?;
        let value: Value = response
            .json()
            .await
            .map_err(|e| format!("模拟面 OCR 响应解析失败：{e}"))?;
        let has_lines = value
            .get("lines")
            .and_then(Value::as_array)
            .map(|lines| !lines.is_empty())
            .unwrap_or(false);
        if has_lines {
            Ok(value)
        } else {
            Err("模拟面 OCR 不可用".to_string())
        }
    }

    async fn click(&mut self, x: i32, y: i32) -> Result<(), String> {
        sim_post_at(&self.base, "click", json!({ "x": x, "y": y })).await?;
        Ok(())
    }

    async fn type_text(&mut self, text: &str) -> Result<(), String> {
        sim_post_at(&self.base, "type", json!({ "text": text })).await?;
        Ok(())
    }

    async fn key(&mut self, key: &str) -> Result<(), String> {
        sim_post_at(&self.base, "key", json!({ "key": key })).await?;
        Ok(())
    }

    async fn launch(&mut self, _target: &str) -> Result<(), String> {
        // 模拟面无独立启动端点；启动语义由真实面/后续接线承载。
        Ok(())
    }
}

/// 感知闭环（指定 surface）：截图/OCR 感知 → 定位锚点 → 门禁动作 → 验证 → 下一步/完成，每步审计。
///
/// 前置：任务已 Approved 或 Running（Pending 返回 Err，需用户先批准）。
/// 敏感 UI（密码/支付/验证码）在每步感知后扫描，命中即 Fused 熔断并要求人工接管。
/// 任一动作被门禁拒绝（未批准/越界应用/超预算/超时）立即停止并返回错误。
pub async fn run_approved_task_on(
    registry: &crate::computer_task::ComputerTaskRegistry,
    audit: &mut crate::audit::AuditLog,
    session_id: &str,
    task_id: &str,
    goals: &[TaskGoal],
    surface: &mut dyn TaskSurface,
) -> Result<TaskReport, String> {
    // 前置门禁：以首个动作做整体授权检查（任务须已批准）。
    let task = registry
        .get(task_id)
        .ok_or_else(|| format!("任务 {task_id} 不存在"))?;
    if task.state != crate::computer_task::TaskState::Approved
        && task.state != crate::computer_task::TaskState::Running
    {
        return Err(format!(
            "任务 {task_id} 状态 {:?}，需用户先批准（Pending→approve）",
            task.state
        ));
    }
    // 当前前台应用（用于门禁的目标应用匹配）。
    let surface_app = surface.app();
    // 闭环步动作名（click/type/key/launch）→ 门禁动作名（desktop_*）。
    let first_action = if goals.is_empty() {
        "desktop_click"
    } else {
        match goals[0].action.as_str() {
            "click" => "desktop_click",
            "type" => "desktop_type",
            "key" => "desktop_key",
            "launch" => "desktop_launch",
            other => other,
        }
    };
    task_gate_check(
        registry,
        Some(&mut *audit),
        session_id,
        task_id,
        first_action,
        &surface_app,
        None,
    )?;
    if registry.get(task_id).unwrap().state != crate::computer_task::TaskState::Running {
        registry.start(task_id)?;
    }
    audit.record(
        session_id,
        "computer_task",
        Some("start".to_string()),
        Some(true),
        format!("任务 {task_id} 感知闭环启动（{} 步目标）", goals.len()),
    );

    let mut steps = 0usize;
    for goal in goals {
        // 1. 感知：OCR 版面。
        let ocr = surface.ocr().await?;
        // 2. 敏感扫描（整屏）→ 熔断。
        if let Some(reason) = scan_ui_sensitive(&ocr) {
            let detail = format!("敏感熔断（第 {} 步感知）：{reason}", steps + 1);
            let _ = registry.fuse(task_id, &detail);
            audit.record(
                session_id,
                "computer_task",
                Some("fuse".to_string()),
                Some(false),
                detail.clone(),
            );
            return Err(detail);
        }
        // 3. 定位锚点（OCR lines 中找目标文本行）。
        let line = find_ocr_line(&ocr, &goal.anchor_text, "")
            .ok_or_else(|| format!("定位失败：未找到锚点「{}」", goal.anchor_text))?;
        let x = line.get("x").and_then(Value::as_i64).unwrap_or(0) as i32;
        let y = line.get("y").and_then(Value::as_i64).unwrap_or(0) as i32;
        let w = line.get("width").and_then(Value::as_i64).unwrap_or(0) as i32;
        let h = line.get("height").and_then(Value::as_i64).unwrap_or(0) as i32;
        let (cx, cy) = (x + w / 2, y + h / 2);
        // 4. 门禁动作（状态/允许集/目标应用/敏感/预算，拒绝即停）。
        let action_name = match goal.action.as_str() {
            "click" => "desktop_click",
            "type" => "desktop_type",
            "key" => "desktop_key",
            "launch" => "desktop_launch",
            other => return Err(format!("闭环不支持的动作：{other}")),
        };
        task_gate_check(
            registry,
            Some(&mut *audit),
            session_id,
            task_id,
            action_name,
            &surface_app,
            None,
        )?;
        let outcome = match goal.action.as_str() {
            "click" => surface.click(cx, cy).await,
            "type" => surface.type_text(&goal.value).await,
            "key" => surface.key(&goal.value).await,
            "launch" => surface.launch(&goal.value).await,
            _ => unreachable!(),
        };
        outcome.map_err(|e| format!("第 {} 步动作执行失败：{e}", steps + 1))?;
        registry.record_action(task_id);
        steps += 1;
        // 5. 验证（可选）。
        if let Some(expected) = &goal.verify_text {
            let verified = verify_text_appears_on(surface, expected).await?;
            if !verified {
                return Err(format!("验证失败（第 {steps} 步）：未出现「{expected}」"));
            }
        }
        audit.record(
            session_id,
            "computer_task",
            Some("step".to_string()),
            Some(true),
            format!("任务 {task_id} 第 {steps} 步完成（{action_name}）@({cx},{cy})"),
        );
        // 6. 超时兜底（超时自动暂停并报错）。
        registry.check_can_execute(task_id)?;
    }
    let _ = registry.complete(task_id);
    audit.record(
        session_id,
        "computer_task",
        Some("complete".to_string()),
        Some(true),
        format!("任务 {task_id} 闭环完成，共 {steps} 步"),
    );
    Ok(TaskReport {
        task_id: task_id.to_string(),
        steps,
        state: crate::computer_task::TaskState::Completed,
        detail: "全部目标完成".to_string(),
    })
}

/// 感知闭环（模拟/沙箱面便捷入口）：`OWO_SIM_QQ_URL` 指向 owo-sim-qq，
/// 在沙箱应用内跑通"打开应用→输入→保存/发送→验证"最小闭环。
///
/// 真实桌面需显式授权（本轮不提供）；逻辑与 [`run_approved_task_on`] 完全一致。
pub async fn run_approved_task(
    registry: &crate::computer_task::ComputerTaskRegistry,
    audit: &mut crate::audit::AuditLog,
    session_id: &str,
    task_id: &str,
    goals: &[TaskGoal],
) -> Result<TaskReport, String> {
    let mut surface = SimTaskSurface::new()?;
    run_approved_task_on(registry, audit, session_id, task_id, goals, &mut surface).await
}

/// 验证 `needle` 是否出现在 surface 的 OCR 版面中（重试 5 次，间隔 50ms）。
async fn verify_text_appears_on(
    surface: &mut dyn TaskSurface,
    needle: &str,
) -> Result<bool, String> {
    for _ in 0..5 {
        if let Ok(ocr) = surface.ocr().await {
            if find_ocr_line(&ocr, needle, "").is_some() {
                return Ok(true);
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(false)
}

pub struct ScreenOcrTool;

#[async_trait]
impl Tool for ScreenOcrTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "screen_ocr".into(),
            description: "截取当前屏幕（或模拟窗口）并做本地 OCR，返回整行文本 lines（含坐标和 role_hint=button/input/header/text）。定位控件优先用本工具：找到目标行后点击该行中心。不要用 ocr_region 代替本工具".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "max_boxes": { "type": "integer", "description": "最多返回多少词框（默认 0：不带 boxes，避免超大结果影响多轮工具调用；lines 已含坐标）" }
                }
            }),
        }
    }

    async fn run(&self, _ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let max_boxes = args.get("max_boxes").and_then(Value::as_u64).unwrap_or(0) as usize;
        ocr_screen(max_boxes).await
    }
}

pub struct OcrRegionTool;

#[async_trait]
impl Tool for OcrRegionTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ocr_region".into(),
            description: "仅当需要放大识别小字/弹窗时才用（可传 scale 放大）；正常情况下定位控件请用 screen_ocr 的 lines".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "x": { "type": "integer" },
                    "y": { "type": "integer" },
                    "width": { "type": "integer" },
                    "height": { "type": "integer" },
                    "scale": { "type": "integer", "description": "放大倍数，默认 2" }
                },
                "required": ["x", "y", "width", "height"]
            }),
        }
    }

    async fn run(&self, _ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let x = args.get("x").and_then(Value::as_i64).ok_or("缺少 x")? as i32;
        let y = args.get("y").and_then(Value::as_i64).ok_or("缺少 y")? as i32;
        let width = args
            .get("width")
            .and_then(Value::as_i64)
            .ok_or("缺少 width")? as i32;
        let height = args
            .get("height")
            .and_then(Value::as_i64)
            .ok_or("缺少 height")? as i32;
        let scale = args.get("scale").and_then(Value::as_u64).unwrap_or(2) as u32;
        if on_sim_surface() {
            if let Some(mut result) = sim_ocr_lines().await {
                let lines = result.get("lines").cloned().unwrap_or_else(|| json!([]));
                let filtered: Vec<Value> = lines
                    .as_array()
                    .map(|array| {
                        array
                            .iter()
                            .filter(|line| {
                                let line_x =
                                    line.get("x").and_then(Value::as_i64).unwrap_or(0) as i32;
                                let line_y =
                                    line.get("y").and_then(Value::as_i64).unwrap_or(0) as i32;
                                let line_w =
                                    line.get("width").and_then(Value::as_i64).unwrap_or(0) as i32;
                                let line_h =
                                    line.get("height").and_then(Value::as_i64).unwrap_or(0) as i32;
                                line_x < x + width
                                    && line_x + line_w > x
                                    && line_y < y + height
                                    && line_y + line_h > y
                            })
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                let text: String = filtered
                    .iter()
                    .filter_map(|line| line.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join(" ");
                result["lines"] = json!(filtered);
                result["text"] = json!(text);
                result["chars"] = json!(text.chars().count());
                if let Value::Object(map) = &mut result {
                    map.insert("surface".into(), json!("sim"));
                    map.insert("note".into(), json!("坐标为模拟窗口坐标"));
                }
                return Ok(result);
            }
        }
        let bmp = if on_sim_surface() {
            sim_fetch_frame().await?
        } else {
            crate::platform::capture_screen().ok_or("屏幕截图失败")?
        };
        let cropped = crate::ocr::crop_scale_bmp(&bmp, x, y, width, height, scale)
            .map_err(|e| format!("区域裁剪失败：{e}"))?;
        let summary = crate::paddle_ocr::ocr_preferred(&cropped)
            .await
            .map_err(|e| format!("区域 OCR 失败：{e}"))?;
        let mut result = ocr_summary_json(&summary, 200);
        if let Value::Object(map) = &mut result {
            map.insert(
                "surface".into(),
                json!(if on_sim_surface() { "sim" } else { "desktop" }),
            );
        }
        Ok(result)
    }
}

pub struct DesktopWindowOcrTool;

#[async_trait]
impl Tool for DesktopWindowOcrTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "desktop_window_ocr".into(),
            description: "后台只读抓取指定窗口内容并 OCR（PrintWindow，可抓被遮挡窗口；传 hwnd，或 process/title 模糊匹配），返回窗口屏幕矩形和整行文本（屏幕坐标），用于窗口级情景理解".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "hwnd": { "type": "integer" },
                    "process": { "type": "string" },
                    "title": { "type": "string" }
                }
            }),
        }
    }

    async fn run(&self, _ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let hwnd: isize = if let Some(value) = args.get("hwnd").and_then(Value::as_i64) {
            value as isize
        } else {
            let process = args
                .get("process")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let title = args
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if process.is_empty() && title.is_empty() {
                return Err("desktop_window_ocr 需要 hwnd 或 process/title".to_string());
            }
            let windows = crate::platform::window_list();
            windows
                .iter()
                .find(|window| {
                    window.visible
                        && ((!process.is_empty() && window.process.contains(process))
                            || (!title.is_empty() && window.title.contains(title)))
                })
                .map(|window| window.hwnd)
                .ok_or_else(|| format!("未找到窗口（process={process}, title={title}）"))?
        };
        let (bmp, rect) = crate::platform::capture_window_bmp_deep(hwnd)
            .ok_or_else(|| format!("窗口截图失败（hwnd={hwnd}）"))?;
        let summary = crate::paddle_ocr::ocr_preferred(&bmp).await?;
        let mut result = ocr_summary_json(&summary, 200);
        if let Value::Object(map) = &mut result {
            if let Some(lines) = map.get_mut("lines").and_then(Value::as_array_mut) {
                for line in lines {
                    if let Some(x) = line.get("x").and_then(Value::as_i64) {
                        line["x"] = json!(x + rect.0 as i64);
                    }
                    if let Some(y) = line.get("y").and_then(Value::as_i64) {
                        line["y"] = json!(y + rect.1 as i64);
                    }
                }
            }
            if let Some(boxes) = map.get_mut("boxes").and_then(Value::as_array_mut) {
                for b in boxes {
                    if let Some(x) = b.get("x").and_then(Value::as_i64) {
                        b["x"] = json!(x + rect.0 as i64);
                    }
                    if let Some(y) = b.get("y").and_then(Value::as_i64) {
                        b["y"] = json!(y + rect.1 as i64);
                    }
                }
            }
            map.insert("surface".into(), json!("window"));
            map.insert(
                "window".into(),
                json!({ "hwnd": hwnd, "rect": [rect.0, rect.1, rect.2, rect.3] }),
            );
        }
        Ok(result)
    }
}

pub struct DesktopForegroundTool;

#[async_trait]
impl Tool for DesktopForegroundTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "desktop_foreground".into(),
            description: "返回当前前台窗口的进程名、标题和屏幕矩形".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
        }
    }

    async fn run(&self, _ctx: &mut ToolContext<'_>, _args: Value) -> Result<Value, String> {
        if on_sim_surface() {
            return Ok(json!({
                "process": "owo-sim-qq",
                "title": "OwO 模拟QQ - 张子豪",
                "rect": [0, 0, 1020, 700],
                "surface": "sim",
            }));
        }
        let (app_id, title) =
            crate::platform::poll_foreground_app().ok_or_else(|| "无法获取前台窗口".to_string())?;
        let rect = crate::platform::foreground_window_rect();
        Ok(json!({ "process": app_id, "title": title, "rect": rect }))
    }
}

pub struct DesktopWindowListTool;

#[async_trait]
impl Tool for DesktopWindowListTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "desktop_window_list".into(),
            description:
                "列出当前所有可见顶层窗口（进程名/标题/矩形），用于找到 QQ、浏览器等目标窗口".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
        }
    }

    async fn run(&self, _ctx: &mut ToolContext<'_>, _args: Value) -> Result<Value, String> {
        if on_sim_surface() {
            return Ok(json!({
                "windows": [{
                    "hwnd": 1,
                    "pid": 1,
                    "process": "owo-sim-qq",
                    "title": "OwO 模拟QQ - 张子豪",
                    "rect": [0, 0, 1020, 700],
                    "visible": true,
                }],
                "surface": "sim",
            }));
        }
        let windows = crate::platform::window_list();
        Ok(json!({ "windows": windows }))
    }
}

pub struct DesktopActivateTool;

#[async_trait]
impl Tool for DesktopActivateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "desktop_activate".into(),
            description: "把指定进程名或标题的窗口切到前台（可传 process 或 title，模糊匹配）"
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "process": { "type": "string", "description": "例如 qq / msedge / owo-sim-qq" },
                    "title": { "type": "string", "description": "窗口标题包含文本" }
                }
            }),
        }
    }

    async fn run(&self, _ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        if on_sim_surface() {
            let process = args
                .get("process")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let title = args
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if process.is_empty() && title.is_empty() {
                return Err("desktop_activate 需要 process 或 title".to_string());
            }
            return Ok(json!({ "activated": true, "foreground": "owo-sim-qq", "surface": "sim" }));
        }
        let process = args
            .get("process")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let title = args
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        crate::platform::activate_window(&process, &title)?;
        std::thread::sleep(Duration::from_millis(200));
        let (app_id, title) = crate::platform::poll_foreground_app().unwrap_or_default();
        Ok(json!({ "activated": true, "foreground": app_id, "title": title }))
    }
}

pub struct DesktopClickTool;

#[async_trait]
impl Tool for DesktopClickTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "desktop_click".into(),
            description: "单击鼠标左键：传入 (x, y) 屏幕坐标，或传入元素注册表的 element_id（需先经 vision_ground/screen_ocr 刷新）自动取元素中心"
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "x": { "type": "integer", "description": "屏幕坐标（与 y 同传时使用）" },
                    "y": { "type": "integer" },
                    "element_id": { "type": "string", "description": "窗口元素注册表的稳定元素 ID（与 app_id 同传时优先于坐标）" },
                    "app_id": { "type": "string", "description": "element_id 所属应用标识（如 qq）" }
                }
            }),
        }
    }

    async fn run(&self, ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        if let Some(element_id) = args.get("element_id").and_then(Value::as_str) {
            let app_id = args
                .get("app_id")
                .and_then(Value::as_str)
                .ok_or("缺少 app_id（element_id 需要所属应用）")?;
            let (x, y) = {
                let registry = ctx
                    .elements
                    .lock()
                    .map_err(|_| "窗口元素注册表锁中毒".to_string())?;
                let element = registry.get_by_id(app_id, element_id).ok_or_else(|| {
                    format!("稳定元素 ID 未命中（可能已失效）：{element_id}；请先刷新感知")
                })?;
                (
                    element.x + element.width / 2,
                    element.y + element.height / 2,
                )
            };
            if on_sim_surface() {
                return sim_post("click", json!({ "x": x, "y": y })).await;
            }
            executor::click_at_screen(x, y)?;
            return Ok(json!({
                "clicked": [x, y],
                "element_id": element_id,
                "app_id": app_id,
            }));
        }
        let x = args
            .get("x")
            .and_then(Value::as_i64)
            .ok_or("缺少 x 或 element_id")? as i32;
        let y = args.get("y").and_then(Value::as_i64).ok_or("缺少 y")? as i32;
        if on_sim_surface() {
            return sim_post("click", json!({ "x": x, "y": y })).await;
        }
        executor::click_at_screen(x, y)?;
        Ok(json!({ "clicked": [x, y] }))
    }
}

pub struct DesktopTypeTool;

#[async_trait]
impl Tool for DesktopTypeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "desktop_type".into(),
            description: "向前台窗口注入 UTF-16 文本（不依赖 IME；中文/英文/表情均可）".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            }),
        }
    }

    async fn run(&self, _ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let text = required_string(&args, "text")?;
        if on_sim_surface() {
            return sim_post("type", json!({ "text": text })).await;
        }
        executor::send_unicode(&text)?;
        Ok(json!({ "typed_chars": text.chars().count() }))
    }
}

pub struct DesktopKeyTool;

#[async_trait]
impl Tool for DesktopKeyTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "desktop_key".into(),
            description: "向前台窗口发送单个按键（enter/tab/backspace/delete/escape/space/up/down/left/right/home/end/f1-f24）".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "key": { "type": "string" } },
                "required": ["key"]
            }),
        }
    }

    async fn run(&self, _ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let key = required_string(&args, "key")?;
        if on_sim_surface() {
            return sim_post("key", json!({ "key": key })).await;
        }
        executor::send_shortcut(&key)?;
        Ok(json!({ "key": key }))
    }
}

pub struct DesktopShortcutTool;

#[async_trait]
impl Tool for DesktopShortcutTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "desktop_shortcut".into(),
            description:
                "向前台窗口发送组合键，例如 ctrl+a / ctrl+c / ctrl+v / alt+tab / ctrl+shift+o"
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": { "combo": { "type": "string" } },
                "required": ["combo"]
            }),
        }
    }

    async fn run(&self, _ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let combo = required_string(&args, "combo")?;
        if on_sim_surface() {
            return sim_post("key", json!({ "key": combo })).await;
        }
        executor::send_shortcut(&combo)?;
        Ok(json!({ "combo": combo }))
    }
}

pub struct DesktopLaunchTool;

#[async_trait]
impl Tool for DesktopLaunchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "desktop_launch".into(),
            description: "启动应用（exe 路径）或打开 URL（交给系统默认浏览器）".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "target": { "type": "string" } },
                "required": ["target"]
            }),
        }
    }

    async fn run(&self, _ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let target = required_string(&args, "target")?;
        if on_sim_surface() {
            return Ok(json!({ "launched": target, "surface": "sim" }));
        }
        executor::launch_target(&target)?;
        Ok(json!({ "launched": target }))
    }
}

pub struct DesktopWaitTool;

#[async_trait]
impl Tool for DesktopWaitTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "desktop_wait".into(),
            description: "等待指定毫秒（最多 120000），用于等对方回复/页面加载/动画完成".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "ms": { "type": "integer" } },
                "required": ["ms"]
            }),
        }
    }

    async fn run(&self, _ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let ms = args
            .get("ms")
            .and_then(Value::as_u64)
            .ok_or("缺少 ms")?
            .min(120_000);
        tokio::time::sleep(Duration::from_millis(ms)).await;
        Ok(json!({ "waited_ms": ms }))
    }
}

pub struct DesktopScrollTool;

#[async_trait]
impl Tool for DesktopScrollTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "desktop_scroll".into(),
            description: "把鼠标移到屏幕坐标 (x,y) 并滚动滚轮（delta 正数向上、负数向下，一格 120），用于滚动聊天/列表".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "x": { "type": "integer" },
                    "y": { "type": "integer" },
                    "delta": { "type": "integer" }
                },
                "required": ["x", "y", "delta"]
            }),
        }
    }

    async fn run(&self, _ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let x = args.get("x").and_then(Value::as_i64).ok_or("缺少 x")? as i32;
        let y = args.get("y").and_then(Value::as_i64).ok_or("缺少 y")? as i32;
        let delta = args
            .get("delta")
            .and_then(Value::as_i64)
            .ok_or("缺少 delta")? as i32;
        if on_sim_surface() {
            // 模拟窗口布局固定，无需滚动。
            return Ok(json!({ "scrolled": [x, y, delta], "surface": "sim" }));
        }
        executor::scroll_at_screen(x, y, delta)?;
        Ok(json!({ "scrolled": [x, y, delta] }))
    }
}

pub struct DesktopWaitUntilTool;

#[async_trait]
impl Tool for DesktopWaitUntilTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "desktop_wait_until".into(),
            description: "轮询屏幕 OCR，直到出现包含指定文本的行（可限定 role_hint=button/input/message/header）；用于等待对方回复/页面加载/消息上屏，返回匹配行与坐标；超时返回 matched=false".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "要等待出现的文本" },
                    "role": { "type": "string", "description": "可选：限定行类型 button/input/message/header" },
                    "timeout_ms": { "type": "integer", "description": "最长等待毫秒，默认 30000，最大 120000" },
                    "interval_ms": { "type": "integer", "description": "轮询间隔毫秒，默认 1000" }
                },
                "required": ["text"]
            }),
        }
    }

    async fn run(&self, _ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let needle = required_string(&args, "text")?;
        let role = args
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let timeout_ms = args
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(30_000)
            .min(120_000);
        let interval_ms = args
            .get("interval_ms")
            .and_then(Value::as_u64)
            .unwrap_or(1_000)
            .max(200);
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let mut last_ocr = Value::Null;
        let mut last_error = String::new();
        while Instant::now() < deadline {
            match ocr_screen(0).await {
                Ok(ocr) => {
                    last_ocr = ocr.clone();
                    if let Some(line) = find_ocr_line(&ocr, &needle, &role) {
                        let elapsed = timeout_ms.saturating_sub(
                            deadline
                                .saturating_duration_since(Instant::now())
                                .as_millis() as u64,
                        );
                        return Ok(json!({
                            "matched": true,
                            "text": needle,
                            "line": line,
                            "elapsed_ms": elapsed,
                            "surface": ocr.get("surface").cloned().unwrap_or(json!("unknown")),
                        }));
                    }
                }
                Err(error) => {
                    last_error = error;
                }
            }
            tokio::time::sleep(Duration::from_millis(interval_ms)).await;
        }
        let elapsed_ms = timeout_ms.saturating_sub(
            deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as u64,
        );
        let preview: String = last_ocr
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .chars()
            .take(500)
            .collect();
        Ok(json!({
            "matched": false,
            "text": needle,
            "elapsed_ms": elapsed_ms,
            "surface": last_ocr.get("surface").cloned().unwrap_or(json!("unknown")),
            "last_error": last_error,
            "last_ocr_preview": preview,
        }))
    }
}

pub struct ScreenVisionTool;

/// 视觉面截图：可选区域裁剪+放大（小字/局部验证用）。
async fn capture_vision_png_with_region(args: &Value) -> Result<(Vec<u8>, String), String> {
    if let (Some(x), Some(y), Some(width), Some(height)) = (
        args.get("x").and_then(Value::as_i64),
        args.get("y").and_then(Value::as_i64),
        args.get("width").and_then(Value::as_i64),
        args.get("height").and_then(Value::as_i64),
    ) {
        let scale = args.get("scale").and_then(Value::as_u64).unwrap_or(3) as u32;
        return crate::vision::capture_vision_png_region(
            x as i32,
            y as i32,
            width as i32,
            height as i32,
            scale,
        )
        .await;
    }
    crate::vision::capture_vision_png().await
}

#[async_trait]
impl Tool for ScreenVisionTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "screen_vision".into(),
            description: "把当前屏幕（或模拟窗口）截图交给视觉模型做场景描述（本地 Ollama 或 BYOK 云端）；视觉只用于理解与验证，不直接控制，主控制仍用 screen_ocr".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "可选：自定义描述指令" },
                    "x": { "type": "integer", "description": "可选：区域左上角 x（与 width/height 同传时裁剪放大）" },
                    "y": { "type": "integer" },
                    "width": { "type": "integer" },
                    "height": { "type": "integer" },
                    "scale": { "type": "integer", "description": "区域放大倍数，默认 3" }
                }
            }),
        }
    }

    async fn run(&self, _ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let (png, surface) = capture_vision_png_with_region(&args).await?;
        let prompt = args
            .get("prompt")
            .and_then(Value::as_str)
            .unwrap_or(
                "请用中文描述这个界面的当前状态：这是什么应用？有哪些关键控件（按钮/输入框/消息）？\
                 它们大致在什么位置（给出屏幕坐标范围）？最新消息内容是什么？",
            )
            .to_string();
        let description = crate::vision::describe_image(&png, &prompt).await?;
        let config = crate::vision::VisionConfig::from_env();
        Ok(json!({
            "surface": surface,
            "provider": config.provider,
            "model": config.model,
            "description": description,
        }))
    }
}

pub struct VisionVerifyTool;

#[async_trait]
impl Tool for VisionVerifyTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "vision_verify".into(),
            description: "让视觉模型针对当前截图回答 yes/no 问题（如“消息是否已上屏”“输入框是否已清空”），返回 answer/confidence，用于异步完成验证；默认忽略输入框占位文字".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "question": { "type": "string" },
                    "ignore_placeholder": { "type": "boolean", "description": "是否忽略输入框占位文字（默认 true，避免把“输入消息...”误判为实际内容）" },
                    "x": { "type": "integer", "description": "可选：只验证该区域（裁剪放大）" },
                    "y": { "type": "integer" },
                    "width": { "type": "integer" },
                    "height": { "type": "integer" },
                    "scale": { "type": "integer" }
                },
                "required": ["question"]
            }),
        }
    }

    async fn run(&self, _ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let question = required_string(&args, "question")?;
        let (png, surface) = capture_vision_png_with_region(&args).await?;
        let ignore_placeholder = args
            .get("ignore_placeholder")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let prompt = crate::vision::verification_prompt(&question, ignore_placeholder);
        let raw = crate::vision::describe_image(&png, &prompt).await?;
        let (answer, confidence) = crate::vision::parse_verification(&raw);
        let config = crate::vision::VisionConfig::from_env();
        Ok(json!({
            "surface": surface,
            "provider": config.provider,
            "model": config.model,
            "question": question,
            "answer": answer,
            "confidence": confidence,
            "raw": raw,
        }))
    }
}

pub struct VisionGroundTool;

#[async_trait]
impl Tool for VisionGroundTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "vision_ground".into(),
            description: "让视觉模型定位描述的元素（返回坐标框），并与 OCR 文本交叉验证；matched=true 且 cross_validated=true 时点击 line 中心；matched=true 且 vision_only=true（置信度≥0.9、无 OCR 文本的纯视觉元素，如图片表情/自绘按钮）时只能点击 box 中心；提供 app_id 时结果并入窗口元素注册表并返回稳定 element_id，后续 desktop_click 可直接用 element_id".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "description": { "type": "string", "description": "要定位的元素描述，例如“发送按钮”“输入框”" },
                    "app_id": { "type": "string", "description": "可选：应用标识（如 qq/weixin/notepad），提供时注册到窗口元素注册表" }
                },
                "required": ["description"]
            }),
        }
    }

    async fn run(&self, ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let description = required_string(&args, "description")?;
        let app_id = args
            .get("app_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut result = crate::vision::ground_element(&description).await?;
        if let Some(app_id) = app_id {
            if result
                .get("matched")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                let grounding = vision_grounding_from_value(&result, &description)?;
                let mut registry = ctx
                    .elements
                    .lock()
                    .map_err(|_| "窗口元素注册表锁中毒".to_string())?;
                let element_id =
                    crate::register_vision_grounding(&mut registry, &app_id, grounding);
                if let Some(element_id) = element_id {
                    result["element_id"] = json!(element_id);
                    result["app_id"] = json!(app_id);
                }
            }
        }
        Ok(result)
    }
}

/// 从 vision_ground 返回值构造注册表输入（box=[x,y,w,h]）。
pub fn vision_grounding_from_value(
    value: &serde_json::Value,
    description: &str,
) -> Result<crate::VisionGrounding, String> {
    let r#box = value
        .get("box")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "grounding 结果缺少 box".to_string())?;
    let numbers: Vec<i32> = r#box
        .iter()
        .filter_map(|item| item.as_i64().map(|value| value as i32))
        .collect();
    if numbers.len() != 4 {
        return Err("grounding 结果 box 格式错误".to_string());
    }
    Ok(crate::VisionGrounding {
        description: description.to_string(),
        x: numbers[0],
        y: numbers[1],
        width: numbers[2],
        height: numbers[3],
        confidence: value
            .get("confidence")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.7),
        cross_validated: value
            .get("cross_validated")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

/// 模拟面执行器源：把动作图执行（/learn/execute-package）落到 headless 虚拟窗口，
/// 使“学习沉淀的技能包”可以在模拟环境里复用验证，不触碰真实桌面。
pub struct SimUiActionSource {
    base: String,
    keep: std::sync::Mutex<Vec<serde_json::Value>>,
}

impl SimUiActionSource {
    pub fn new() -> Result<Self, String> {
        let base = sim_base_url().ok_or("模拟环境未配置 OWO_SIM_QQ_URL")?;
        Ok(Self {
            base,
            keep: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn get(&self, path: &str) -> Result<serde_json::Value, String> {
        sim_http_sync(&self.base, "GET", path, None)
    }

    fn post(&self, path: &str, body: serde_json::Value) -> Result<serde_json::Value, String> {
        sim_http_sync(&self.base, "POST", path, Some(&body))
    }

    fn lines(&self) -> Result<Vec<serde_json::Value>, String> {
        let ocr = self.get("ocr")?;
        Ok(ocr
            .get("lines")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    fn line(&self, handle: u64) -> Result<serde_json::Value, String> {
        let keep = self.keep.lock().map_err(|_| "锚点池锁中毒".to_string())?;
        keep.get(handle as usize)
            .cloned()
            .ok_or_else(|| "模拟锚点句柄失效".to_string())
    }

    fn click_line_center(&self, line: &serde_json::Value) -> Result<(), String> {
        let x = line
            .get("x")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0) as i32
            + line
                .get("width")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0) as i32
                / 2;
        let y = line
            .get("y")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0) as i32
            + line
                .get("height")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0) as i32
                / 2;
        self.post("click", json!({ "x": x, "y": y }))?;
        Ok(())
    }
}

impl crate::executor::UiActionSource for SimUiActionSource {
    fn find(&self, anchor: &crate::learn::SemanticAnchor) -> Result<u64, String> {
        let lines = self.lines()?;
        let found = lines
            .iter()
            .find(|line| sim_anchor_matches(line, anchor))
            .cloned()
            .ok_or_else(|| format!("模拟窗口未找到锚点：{}", anchor.name))?;
        let mut keep = self.keep.lock().map_err(|_| "锚点池锁中毒".to_string())?;
        keep.push(found);
        Ok((keep.len() - 1) as u64)
    }

    fn invoke(&self, handle: u64) -> Result<(), String> {
        let line = self.line(handle)?;
        self.click_line_center(&line)
    }

    fn type_text(&self, handle: u64, text: &str) -> Result<(), String> {
        let line = self.line(handle)?;
        if line.get("role_hint").and_then(serde_json::Value::as_str) == Some("input") {
            self.click_line_center(&line)?;
        }
        self.post("type", json!({ "text": text }))?;
        Ok(())
    }

    fn shortcut(&self, combo: &str) -> Result<(), String> {
        self.post("key", json!({ "key": combo }))?;
        Ok(())
    }

    fn launch(&self, _target: &str) -> Result<(), String> {
        Ok(())
    }

    fn click_at(&self, x: i32, y: i32) -> Result<(), String> {
        self.post("click", json!({ "x": x, "y": y }))?;
        Ok(())
    }

    fn verify(&self, predicate: &str) -> Result<bool, String> {
        if let Some(expected) = predicate.strip_prefix("value:") {
            let state = self.get("state")?;
            return Ok(state
                .get("input")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .contains(expected));
        }
        let ocr = self.get("ocr")?;
        let haystack = serde_json::to_string(&ocr).unwrap_or_default();
        if let Some(expected) = predicate.strip_prefix("ui:") {
            return Ok(haystack.contains(expected));
        }
        Ok(haystack.contains(predicate))
    }
}

/// 模拟版面行与语义锚点的匹配规则（供 SimUiActionSource 与单测复用）。
fn sim_anchor_matches(line: &serde_json::Value, anchor: &crate::learn::SemanticAnchor) -> bool {
    let text = line
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let role_hint = line.get("role_hint").and_then(serde_json::Value::as_str);
    let role_ok = match anchor.role.as_deref() {
        Some("button") => role_hint == Some("button"),
        Some("edit") | Some("input") => role_hint == Some("input"),
        Some("text") => matches!(
            role_hint,
            Some("message" | "header" | "contact" | "status" | "preview")
        ),
        Some(_) => true,
        None => true,
    };
    role_ok && !anchor.name.is_empty() && text.contains(&anchor.name)
}

/// 纯 TcpStream 的同步 HTTP 客户端（仅本机模拟服务）：
/// 避免 reqwest::blocking 在 async 处理器中析构内部 runtime 导致 panic。
fn sim_http_sync(
    base: &str,
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
) -> Result<serde_json::Value, String> {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let url = format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| "模拟服务地址仅支持 http://".to_string())?;
    let (host_port, path_and_query) = match rest.split_once('/') {
        Some((host_port, path)) => (host_port, format!("/{path}")),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match host_port.rsplit_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().unwrap_or(80)),
        None => (host_port, 80),
    };
    let payload = body
        .map(|value| serde_json::to_string(value).unwrap_or_default())
        .unwrap_or_default();
    let request = format!(
        "{method} {path_and_query} HTTP/1.1\r\nHost: {host_port}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let mut stream = TcpStream::connect((host, port))
        .map_err(|e| format!("连接模拟服务失败（{host}:{port}）：{e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .map_err(|e| format!("设置读超时失败：{e}"))?;
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("发送模拟请求失败：{e}"))?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|e| format!("读取模拟响应失败：{e}"))?;
    let text = String::from_utf8_lossy(&response).to_string();
    let body_start = text
        .find("\r\n\r\n")
        .map(|index| index + 4)
        .ok_or_else(|| format!("模拟服务响应无正文：{text}"))?;
    let json_body = &text[body_start.min(text.len())..];
    serde_json::from_str(json_body).map_err(|e| format!("模拟服务响应解析失败：{e}：{json_body}"))
}

// ---------- 浏览器工具（Playwright + 本机 Edge） ----------

struct BrowserDriver {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl BrowserDriver {
    async fn call(&mut self, command: &str, args: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({ "id": id, "cmd": command, "args": args });
        let mut line =
            serde_json::to_string(&request).map_err(|e| format!("序列化浏览器命令失败：{e}"))?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("写入浏览器驱动失败：{e}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| format!("刷新浏览器驱动失败：{e}"))?;
        loop {
            let mut response = String::new();
            let read = tokio::time::timeout(
                Duration::from_secs(180),
                self.stdout.read_line(&mut response),
            )
            .await
            .map_err(|_| format!("浏览器命令超时：{command}"))?
            .map_err(|e| format!("读取浏览器驱动失败：{e}"))?;
            if read == 0 {
                return Err(format!("浏览器驱动进程已退出（命令：{command}）"));
            }
            let trimmed = response.trim();
            if trimmed.is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(trimmed)
                .map_err(|e| format!("浏览器驱动返回非法 JSON：{e}：{trimmed}"))?;
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if value.get("ok").and_then(Value::as_bool) == Some(true) {
                return Ok(value.get("data").cloned().unwrap_or(Value::Null));
            }
            return Err(value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("浏览器命令失败")
                .to_string());
        }
    }
}

#[derive(Clone)]
pub struct BrowserTools {
    driver: Arc<AsyncMutex<Option<BrowserDriver>>>,
}

impl BrowserTools {
    pub fn new() -> Self {
        Self {
            driver: Arc::new(AsyncMutex::new(None)),
        }
    }

    async fn call(&self, command: &str, args: Value) -> Result<Value, String> {
        let mut guard = self.driver.lock().await;
        ensure_browser_driver(&mut guard).await?;
        let driver = guard.as_mut().ok_or("浏览器驱动未启动")?;
        let result = driver.call(command, args).await;
        if result.is_err() {
            // 驱动异常时清理，下次调用重新拉起。
            if let Some(child) = guard.as_mut() {
                let _ = child.child.kill().await;
            }
            *guard = None;
        }
        result
    }
}

impl Default for BrowserTools {
    fn default() -> Self {
        Self::new()
    }
}

async fn ensure_browser_driver(lock: &mut Option<BrowserDriver>) -> Result<(), String> {
    if lock.is_some() {
        return Ok(());
    }
    let (node, node_path) = node_runtime();
    let script = include_str!("../../../scripts/browser-driver.js");
    let temp_dir = std::env::temp_dir().join("owo-agent-browser");
    std::fs::create_dir_all(&temp_dir).map_err(|e| format!("创建浏览器驱动目录失败：{e}"))?;
    let script_path = temp_dir.join("browser-driver.js");
    std::fs::write(&script_path, script).map_err(|e| format!("写出浏览器驱动失败：{e}"))?;
    let profile = std::env::var("OWO_BROWSER_PROFILE").unwrap_or_else(|_| {
        let local = std::env::var("LOCALAPPDATA")
            .unwrap_or_else(|_| temp_dir.to_string_lossy().to_string());
        format!("{}\\OwO\\Agent\\browser-profile", local)
    });
    let mut command = Command::new(&node);
    command
        .arg(&script_path)
        .env("OWO_BROWSER_PROFILE", profile)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(node_path) = node_path {
        command.env("NODE_PATH", node_path);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("启动浏览器驱动失败（node={node}）：{e}"))?;
    let stdin = child.stdin.take().ok_or("浏览器驱动 stdin 不可用")?;
    let stdout = child.stdout.take().ok_or("浏览器驱动 stdout 不可用")?;
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                eprintln!("[browser-driver] {}", line.trim_end());
                line.clear();
            }
        });
    }
    *lock = Some(BrowserDriver {
        child,
        stdin,
        stdout: BufReader::new(stdout),
        next_id: 1,
    });
    Ok(())
}

fn node_runtime() -> (String, Option<String>) {
    if let Ok(node) = std::env::var("OWO_BROWSER_NODE") {
        let node_path = std::env::var("OWO_BROWSER_NODE_PATH").ok();
        return (node, node_path);
    }
    if let Ok(node) = std::env::var("OWO_SKILL_NODE") {
        let runtime = std::env::var("OWO_SKILL_RUNTIME").unwrap_or_default();
        let node_path = if runtime.is_empty() {
            None
        } else {
            Some(format!("{}\\node\\node_modules", runtime))
        };
        return (node, node_path);
    }
    const FALLBACK_NODE: &str = r"C:\Users\23843\.cache\codex-runtimes\codex-primary-runtime\dependencies\node\bin\node.exe";
    const FALLBACK_NODE_PATH: &str = r"C:\Users\23843\.cache\codex-runtimes\codex-primary-runtime\dependencies\node\node_modules";
    if Path::new(FALLBACK_NODE).exists() {
        return (
            FALLBACK_NODE.to_string(),
            Some(FALLBACK_NODE_PATH.to_string()),
        );
    }
    ("node".to_string(), None)
}

macro_rules! browser_tool {
    ($tool:ident, $name:literal, $description:literal, $schema:expr) => {
        pub struct $tool {
            pub tools: BrowserTools,
        }

        #[async_trait]
        impl Tool for $tool {
            fn spec(&self) -> ToolSpec {
                ToolSpec {
                    name: $name.into(),
                    description: $description.into(),
                    input_schema: $schema,
                }
            }

            async fn run(&self, _ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
                self.tools
                    .call($name.trim_start_matches("browser_"), args)
                    .await
            }
        }
    };
}

browser_tool!(
    BrowserNavigateTool,
    "browser_navigate",
    "在浏览器中打开 URL（持久化 Edge 窗口，保持登录态）",
    json!({ "type": "object", "properties": { "url": { "type": "string" } }, "required": ["url"] })
);

browser_tool!(
    BrowserSearchTool,
    "browser_search",
    "用 Bing/Baidu 搜索关键词并返回结果列表（标题/链接/摘要）",
    json!({ "type": "object", "properties": { "query": { "type": "string" }, "engine": { "type": "string", "enum": ["bing", "baidu"] } }, "required": ["query"] })
);

browser_tool!(
    BrowserSnapshotTool,
    "browser_snapshot",
    "读取当前页面的可见文本、链接、图片和输入框清单，用于理解页面状态",
    json!({ "type": "object", "properties": { "max_items": { "type": "integer" } } })
);

browser_tool!(
    BrowserClickTool,
    "browser_click",
    "点击页面元素：传 selector（CSS 选择器）或 text（页面可见文本）",
    json!({ "type": "object", "properties": { "selector": { "type": "string" }, "text": { "type": "string" }, "exact": { "type": "boolean" } } })
);

browser_tool!(
    BrowserTypeTool,
    "browser_type",
    "在页面输入框填文本：传 selector 时自动聚焦填充，否则向当前焦点输入",
    json!({ "type": "object", "properties": { "selector": { "type": "string" }, "text": { "type": "string" } }, "required": ["text"] })
);

browser_tool!(
    BrowserPressTool,
    "browser_press",
    "向页面发送按键，例如 Enter / Escape / Tab / Control+A",
    json!({ "type": "object", "properties": { "key": { "type": "string" } }, "required": ["key"] })
);

// 需要写工作区的浏览器工具：路径校验 + 绝对路径传给驱动。
pub struct BrowserScreenshotWriteTool {
    pub tools: BrowserTools,
}

#[async_trait]
impl Tool for BrowserScreenshotWriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "browser_screenshot".into(),
            description: "把当前页面截图保存到工作区路径，返回文件大小".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "path": { "type": "string" }, "full_page": { "type": "boolean" } },
                "required": ["path"]
            }),
        }
    }

    async fn run(&self, ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let path = required_string(&args, "path")?;
        let abs = resolve_session_path(ctx, &path)?;
        let mut call_args = args.clone();
        call_args["path"] = json!(abs.to_string_lossy());
        self.tools.call("screenshot", call_args).await
    }
}

pub struct BrowserDownloadImageWriteTool {
    pub tools: BrowserTools,
}

#[async_trait]
impl Tool for BrowserDownloadImageWriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "browser_download_image".into(),
            description: "下载图片到工作区：传 url 直接下载，或传 src（CSS 选择器）取页面中该图片的地址再下载".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "url": { "type": "string" }, "src": { "type": "string" }, "path": { "type": "string" } },
                "required": ["path"]
            }),
        }
    }

    async fn run(&self, ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let path = required_string(&args, "path")?;
        let abs = resolve_session_path(ctx, &path)?;
        let mut call_args = args.clone();
        call_args["path"] = json!(abs.to_string_lossy());
        self.tools.call("download_image", call_args).await
    }
}

pub struct BrowserCloseTool {
    pub tools: BrowserTools,
}

#[async_trait]
impl Tool for BrowserCloseTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "browser_close".into(),
            description: "关闭浏览器会话（清空页面状态）".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
        }
    }

    async fn run(&self, _ctx: &mut ToolContext<'_>, _args: Value) -> Result<Value, String> {
        let mut guard = self.tools.driver.lock().await;
        if let Some(driver) = guard.as_mut() {
            let _ = driver.call("close", json!({})).await;
            let _ = driver.child.kill().await;
        }
        *guard = None;
        Ok(json!({ "closed": true }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_tool_names_map_to_driver_commands() {
        // 宏按“browser_”前缀去映射驱动命令，这里验证命名契约。
        let names = [
            "browser_navigate",
            "browser_search",
            "browser_snapshot",
            "browser_click",
            "browser_type",
            "browser_press",
        ];
        for name in names {
            let cmd = name.trim_start_matches("browser_");
            assert!(matches!(
                cmd,
                "navigate" | "search" | "snapshot" | "click" | "type" | "press"
            ));
        }
    }

    #[test]
    fn node_runtime_falls_back_without_panicking() {
        let (node, node_path) = node_runtime();
        assert!(!node.is_empty());
        let _ = node_path;
    }

    #[test]
    fn find_ocr_line_matches_text_and_role_filter() {
        let ocr = json!({
            "lines": [
                { "text": "发送", "x": 0, "y": 0, "width": 10, "height": 10, "role_hint": "button" },
                { "text": "对方正在输入…", "x": 0, "y": 20, "width": 10, "height": 10, "role_hint": "status" },
                { "text": "今晚吃什么", "x": 0, "y": 40, "width": 10, "height": 10, "role_hint": "message" }
            ]
        });
        let button = find_ocr_line(&ocr, "发送", "button").expect("应匹配发送按钮");
        assert_eq!(button["role_hint"], "button");
        assert!(find_ocr_line(&ocr, "发送", "message").is_none());
        assert!(find_ocr_line(&ocr, "输入中", "").is_none());
        let message = find_ocr_line(&ocr, "吃什么", "").expect("应匹配消息行");
        assert_eq!(message["y"], 40);
    }

    #[test]
    fn find_ocr_line_is_case_insensitive() {
        let ocr = json!({
            "lines": [{ "text": "Hello World", "x": 0, "y": 0, "width": 10, "height": 10, "role_hint": "text" }]
        });
        assert!(find_ocr_line(&ocr, "hello", "").is_some());
    }

    #[test]
    fn sim_anchor_matches_line_text_and_role() {
        use crate::learn::SemanticAnchor;
        let input = json!({ "text": "输入消息...", "role_hint": "input" });
        let send = json!({ "text": "发送", "role_hint": "button" });
        let message = json!({ "text": "今晚吃什么", "role_hint": "message" });
        assert!(sim_anchor_matches(
            &input,
            &SemanticAnchor {
                app_id: None,
                role: Some("edit".into()),
                name: "输入消息".into(),
                parent: None,
                element_id: None,
            }
        ));
        assert!(sim_anchor_matches(
            &send,
            &SemanticAnchor {
                app_id: None,
                role: Some("button".into()),
                name: "发送".into(),
                parent: None,
                element_id: None,
            }
        ));
        assert!(!sim_anchor_matches(
            &send,
            &SemanticAnchor {
                app_id: None,
                role: Some("input".into()),
                name: "发送".into(),
                parent: None,
                element_id: None,
            }
        ));
        assert!(sim_anchor_matches(
            &message,
            &SemanticAnchor {
                app_id: None,
                role: Some("text".into()),
                name: "吃什么".into(),
                parent: None,
                element_id: None,
            }
        ));
        assert!(!sim_anchor_matches(
            &message,
            &SemanticAnchor {
                app_id: None,
                role: None,
                name: String::new(),
                parent: None,
                element_id: None,
            }
        ));
    }

    #[test]
    fn vision_grounding_from_value_parses_box_and_flags() {
        let value = json!({
            "matched": true,
            "description": "发送按钮",
            "box": [815, 624, 170, 36],
            "confidence": 0.88,
            "cross_validated": true,
            "surface": "desktop"
        });
        let grounding = vision_grounding_from_value(&value, "发送按钮").expect("解析成功");
        assert_eq!(grounding.x, 815);
        assert_eq!(grounding.y, 624);
        assert_eq!(grounding.width, 170);
        assert_eq!(grounding.height, 36);
        assert!(grounding.cross_validated);
        assert!((grounding.confidence - 0.88).abs() < 1e-9);

        let bad = json!({ "matched": true, "box": [1, 2, 3] });
        assert!(vision_grounding_from_value(&bad, "x").is_err());

        let no_confidence = json!({ "matched": true, "box": [1, 2, 3, 4] });
        let grounding = vision_grounding_from_value(&no_confidence, "x").expect("默认置信度");
        assert!((grounding.confidence - 0.7).abs() < 1e-9);
    }
}
