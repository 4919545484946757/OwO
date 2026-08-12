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
    let lines: Vec<Value> = crate::ocr::group_ocr_lines(&summary.boxes)
        .into_iter()
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
        "text": summary.text,
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
                    "max_boxes": { "type": "integer", "description": "最多返回多少词框（默认 120）" }
                }
            }),
        }
    }

    async fn run(&self, _ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let max_boxes = args.get("max_boxes").and_then(Value::as_u64).unwrap_or(120) as usize;
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
            description: "在屏幕坐标 (x, y) 处单击鼠标左键（坐标来自 screen_ocr 的词框中心）"
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "x": { "type": "integer" },
                    "y": { "type": "integer" }
                },
                "required": ["x", "y"]
            }),
        }
    }

    async fn run(&self, _ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let x = args.get("x").and_then(Value::as_i64).ok_or("缺少 x")? as i32;
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
            match ocr_screen(200).await {
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

#[async_trait]
impl Tool for ScreenVisionTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "screen_vision".into(),
            description: "把当前屏幕（或模拟窗口）截图交给视觉模型做场景描述（本地 Ollama 或 BYOK 云端）；视觉只用于理解与验证，不直接控制，主控制仍用 screen_ocr".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "可选：自定义描述指令" }
                }
            }),
        }
    }

    async fn run(&self, _ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let (png, surface) = crate::vision::capture_vision_png().await?;
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
            description: "让视觉模型针对当前截图回答 yes/no 问题（如“消息是否已上屏”“输入框是否已清空”），返回 answer/confidence，用于异步完成验证".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "question": { "type": "string" } },
                "required": ["question"]
            }),
        }
    }

    async fn run(&self, _ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let question = required_string(&args, "question")?;
        let (png, surface) = crate::vision::capture_vision_png().await?;
        let prompt = format!(
            "请只看这张截图回答问题。先回答 YES 或 NO，再给出 0-1 置信度。问题：{question}"
        );
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
            description: "让视觉模型定位描述的元素（返回坐标框），并与 OCR 文本交叉验证；只有 matched=true 且 cross_validated=true 时才可点击返回的 line 中心；这是兜底定位，优先用 screen_ocr".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "description": { "type": "string", "description": "要定位的元素描述，例如“发送按钮”“输入框”" }
                },
                "required": ["description"]
            }),
        }
    }

    async fn run(&self, _ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let description = required_string(&args, "description")?;
        crate::vision::ground_element(&description).await
    }
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
            }
        ));
        assert!(sim_anchor_matches(
            &send,
            &SemanticAnchor {
                app_id: None,
                role: Some("button".into()),
                name: "发送".into(),
                parent: None,
            }
        ));
        assert!(!sim_anchor_matches(
            &send,
            &SemanticAnchor {
                app_id: None,
                role: Some("input".into()),
                name: "发送".into(),
                parent: None,
            }
        ));
        assert!(sim_anchor_matches(
            &message,
            &SemanticAnchor {
                app_id: None,
                role: Some("text".into()),
                name: "吃什么".into(),
                parent: None,
            }
        ));
        assert!(!sim_anchor_matches(
            &message,
            &SemanticAnchor {
                app_id: None,
                role: None,
                name: String::new(),
                parent: None,
            }
        ));
    }
}
