//! 操作学习执行引擎（v0.4 P3）：按动作图执行流程技能包。
//!
//! 核心是“语义锚点 → 动作 → 验证”的图遍历，通过 `UiActionSource` 抽象，
//! Windows 实现走 UI Automation + SendInput；测试用脚本化假源覆盖全部逻辑。

use crate::learn::{ActionGraph, ActionType, SemanticAnchor};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecStep {
    pub node_id: String,
    pub action: String,
    pub anchor: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecReport {
    pub ok: bool,
    pub steps: Vec<ExecStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 界面动作源：Windows 实现（UIA + SendInput）或测试脚本源。
pub trait UiActionSource: Send + Sync {
    fn find(&self, anchor: &SemanticAnchor) -> Result<u64, String>;
    fn invoke(&self, handle: u64) -> Result<(), String>;
    fn type_text(&self, handle: u64, text: &str) -> Result<(), String>;
    fn shortcut(&self, combo: &str) -> Result<(), String>;
    fn launch(&self, target: &str) -> Result<(), String>;
    fn click_at(&self, x: i32, y: i32) -> Result<(), String>;
    fn verify(&self, predicate: &str) -> Result<bool, String>;
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

fn fill_template(template: &str, variables: &HashMap<String, String>) -> String {
    let mut output = template.to_string();
    for (name, value) in variables {
        output = output.replace(&format!("{{{name}}}"), value);
    }
    output
}

/// 按动作图执行：从 start 沿边线性推进，直到无出边或达到步数上限。
pub fn execute_graph(
    source: &dyn UiActionSource,
    graph: &ActionGraph,
    variables: &HashMap<String, String>,
    max_steps: usize,
) -> ExecReport {
    if let Err(error) = graph.validate() {
        return ExecReport {
            ok: false,
            steps: Vec::new(),
            error: Some(error),
        };
    }
    let mut steps = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut current = graph.start.clone();
    let mut error = None;

    for _ in 0..max_steps.max(1) {
        if !visited.insert(current.clone()) {
            error = Some(format!("动作图成环：{current}"));
            break;
        }
        let Some(node) = graph.nodes.iter().find(|node| node.id == current) else {
            error = Some(format!("节点不存在：{current}"));
            break;
        };
        if keyword_breaks(&node.anchor) {
            error = Some("敏感面熔断：密码/支付/验证码等场景不执行".to_string());
            steps.push(ExecStep {
                node_id: node.id.clone(),
                action: "blocked".to_string(),
                anchor: node.anchor.name.clone(),
                status: "blocked".to_string(),
                detail: error.clone().unwrap_or_default(),
            });
            break;
        }

        let action_label = format!("{:?}", node.action_type).to_lowercase();
        let text = node
            .value_template
            .as_deref()
            .map(|template| fill_template(template, variables))
            .unwrap_or_default();
        let outcome = match node.action_type {
            ActionType::Click => source
                .find(&node.anchor)
                .and_then(|handle| source.invoke(handle)),
            ActionType::Type => source
                .find(&node.anchor)
                .and_then(|handle| source.type_text(handle, &text)),
            ActionType::Shortcut => source.shortcut(&text),
            ActionType::Inject => source.type_text(0, &text),
            ActionType::Launch => source.launch(&text),
            ActionType::ClickAt => parse_click_at(&text).and_then(|(x, y)| source.click_at(x, y)),
        };
        let mut ok = outcome.is_ok();
        let mut detail = outcome.err().unwrap_or_default();

        if let Some(predicate) = node.verify.as_deref() {
            match source.verify(predicate) {
                Ok(true) => {}
                Ok(false) => {
                    ok = false;
                    detail = format!("验证失败：{predicate}");
                }
                Err(verify_error) => {
                    ok = false;
                    detail = verify_error;
                }
            }
        }
        steps.push(ExecStep {
            node_id: node.id.clone(),
            action: action_label,
            anchor: node.anchor.name.clone(),
            status: if ok {
                "ok".to_string()
            } else {
                "failed".to_string()
            },
            detail,
        });
        if !ok {
            error = Some(
                steps
                    .last()
                    .map(|step| step.detail.clone())
                    .unwrap_or_default(),
            );
            break;
        }

        let Some(edge) = graph.edges.iter().find(|edge| edge.from == current) else {
            break; // 无出边：完成
        };
        if let Some(predicate) = edge.verify.as_deref() {
            match source.verify(predicate) {
                Ok(true) => {}
                Ok(false) => {
                    error = Some(format!("边验证失败：{predicate}"));
                    break;
                }
                Err(verify_error) => {
                    error = Some(verify_error);
                    break;
                }
            }
        }
        current = edge.to.clone();
    }

    ExecReport {
        ok: error.is_none(),
        steps,
        error,
    }
}

// ---------- Windows 实现：UI Automation + SendInput ----------

#[cfg(target_os = "windows")]
pub struct WindowsUiaSource {
    automation: windows::Win32::UI::Accessibility::IUIAutomation,
    root: windows::Win32::UI::Accessibility::IUIAutomationElement,
    keep: std::sync::Mutex<Vec<windows::Win32::UI::Accessibility::IUIAutomationElement>>,
}

// UIA COM 对象在桌面会话中可跨线程使用（由系统线程模型保证）。
#[cfg(target_os = "windows")]
unsafe impl Send for WindowsUiaSource {}
#[cfg(target_os = "windows")]
unsafe impl Sync for WindowsUiaSource {}

#[cfg(target_os = "windows")]
impl WindowsUiaSource {
    pub fn new() -> Result<Self, String> {
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
            COINIT_DISABLE_OLE1DDE,
        };
        use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation};
        use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
            let automation: IUIAutomation =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                    .map_err(|error| error.to_string())?;
            let hwnd = GetForegroundWindow();
            if hwnd.0.is_null() {
                return Err("无前台窗口".to_string());
            }
            let root = automation
                .ElementFromHandle(hwnd)
                .map_err(|error| error.to_string())?;
            Ok(Self {
                automation,
                root,
                keep: std::sync::Mutex::new(Vec::new()),
            })
        }
    }

    fn find_recursive(
        &self,
        element: &windows::Win32::UI::Accessibility::IUIAutomationElement,
        anchor: &SemanticAnchor,
        depth: u32,
        ancestors: &[String],
    ) -> Option<windows::Win32::UI::Accessibility::IUIAutomationElement> {
        use windows::Win32::UI::Accessibility::TreeScope_Children;

        unsafe {
            let name = element
                .CurrentName()
                .map(|value| value.to_string())
                .unwrap_or_default();
            let role = element
                .CurrentControlType()
                .map(|value| value.0)
                .unwrap_or(0);
            if anchor_matches(anchor, &name, role) && parent_matches(anchor, ancestors) {
                return Some(element.clone());
            }
            if depth >= 8 {
                return None;
            }
            if let Ok(condition) = self.automation.CreateTrueCondition() {
                if let Ok(children) = element.FindAll(TreeScope_Children, &condition) {
                    if let Ok(length) = children.Length() {
                        let mut child_ancestors = ancestors.to_vec();
                        child_ancestors.push(name.clone());
                        for index in 0..length {
                            if let Ok(child) = children.GetElement(index) {
                                if let Some(found) =
                                    self.find_recursive(&child, anchor, depth + 1, &child_ancestors)
                                {
                                    return Some(found);
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }
}

#[cfg(target_os = "windows")]
fn anchor_matches(anchor: &SemanticAnchor, name: &str, role: i32) -> bool {
    let name_ok = anchor.name.is_empty()
        || name.contains(&anchor.name)
        || (!name.trim().is_empty() && anchor.name.contains(name.trim()));
    let role_ok = match anchor.role.as_deref() {
        Some("button") => role == 50000,
        Some("edit") | Some("input") => role == 50004,
        Some("text") => role == 50020,
        Some("list") => role == 50008,
        Some("window") => role == 50032,
        Some("pane") => role == 50033,
        Some(_) => true,
        None => true,
    };
    name_ok && role_ok
}

fn parent_matches(anchor: &SemanticAnchor, ancestors: &[String]) -> bool {
    match anchor.parent.as_deref() {
        None => true,
        Some(parent) => ancestors.iter().any(|ancestor| ancestor.contains(parent)),
    }
}

#[cfg(target_os = "windows")]
impl UiActionSource for WindowsUiaSource {
    fn find(&self, anchor: &SemanticAnchor) -> Result<u64, String> {
        let found = self
            .find_recursive(&self.root, anchor, 0, &[])
            .ok_or_else(|| format!("未找到语义锚点：{}", anchor.name))?;
        let mut keep = self.keep.lock().map_err(|_| "锚点池锁中毒".to_string())?;
        keep.push(found);
        Ok((keep.len() - 1) as u64)
    }

    fn invoke(&self, handle: u64) -> Result<(), String> {
        use windows::Win32::UI::Accessibility::{IUIAutomationInvokePattern, UIA_InvokePatternId};
        let keep = self.keep.lock().map_err(|_| "锚点池锁中毒".to_string())?;
        let element = keep
            .get(handle as usize)
            .ok_or_else(|| "锚点句柄失效".to_string())?;
        unsafe {
            if let Ok(pattern) =
                element.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
            {
                pattern.Invoke().map_err(|error| error.to_string())
            } else {
                click_element(element)
            }
        }
    }

    fn type_text(&self, handle: u64, text: &str) -> Result<(), String> {
        if handle != 0 {
            let keep = self.keep.lock().map_err(|_| "锚点池锁中毒".to_string())?;
            let element = keep
                .get(handle as usize)
                .ok_or_else(|| "锚点句柄失效".to_string())?;
            unsafe {
                let _ = element.SetFocus();
            }
        }
        send_unicode(text)
    }

    fn shortcut(&self, combo: &str) -> Result<(), String> {
        send_shortcut(combo)
    }

    fn launch(&self, target: &str) -> Result<(), String> {
        launch_target(target)
    }

    fn click_at(&self, x: i32, y: i32) -> Result<(), String> {
        click_at_screen(x, y)
    }

    fn verify(&self, predicate: &str) -> Result<bool, String> {
        if let Some(expected) = predicate.strip_prefix("value:") {
            let value = self.focused_edit_value()?;
            return Ok(value.contains(expected));
        }
        if let Some(expected) = predicate.strip_prefix("ui:") {
            let nodes = crate::accessibility::foreground_ui_tree(8, 500).unwrap_or_default();
            return Ok(nodes.iter().any(|node| node.name.contains(expected)));
        }
        let title = crate::platform::foreground_title();
        Ok(title
            .map(|title| title.contains(predicate))
            .unwrap_or(false))
    }
}

#[cfg(target_os = "windows")]
impl WindowsUiaSource {
    /// 读取前台窗口内第一个可编辑控件的当前值（ValuePattern），用于“输入后回读验证”。
    fn focused_edit_value(&self) -> Result<String, String> {
        self.find_value_edit(&self.root, 0)
            .ok_or_else(|| "未找到可读回的可编辑控件".to_string())
    }

    /// 递归查找第一个带 ValuePattern 且当前值非空的控件（兼容 Edit/Document 等类型）。
    fn find_value_edit(
        &self,
        element: &windows::Win32::UI::Accessibility::IUIAutomationElement,
        depth: u32,
    ) -> Option<String> {
        use windows::Win32::UI::Accessibility::{
            IUIAutomationValuePattern, TreeScope_Children, UIA_ValuePatternId,
        };
        unsafe {
            if let Ok(pattern) =
                element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            {
                if let Ok(value) = pattern.CurrentValue() {
                    let text = value.to_string();
                    if !text.trim().is_empty() {
                        return Some(text);
                    }
                }
            }
            if depth >= 8 {
                return None;
            }
            if let Ok(condition) = self.automation.CreateTrueCondition() {
                if let Ok(children) = element.FindAll(TreeScope_Children, &condition) {
                    if let Ok(length) = children.Length() {
                        for index in 0..length {
                            if let Ok(child) = children.GetElement(index) {
                                if let Some(value) = self.find_value_edit(&child, depth + 1) {
                                    return Some(value);
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }
}

#[cfg(target_os = "windows")]
unsafe fn click_element(
    element: &windows::Win32::UI::Accessibility::IUIAutomationElement,
) -> Result<(), String> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
        MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE, MOUSEINPUT,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics;

    let mut point = POINT::default();
    if !element
        .GetClickablePoint(&mut point)
        .map_err(|error| error.to_string())?
        .as_bool()
    {
        return Err("元素不可点击".to_string());
    }
    let width =
        GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SYSTEM_METRICS_INDEX(0)) as f64;
    let height =
        GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SYSTEM_METRICS_INDEX(1)) as f64;
    if width <= 0.0 || height <= 0.0 {
        return Err("无法获取屏幕尺寸".to_string());
    }
    let x = ((point.x as f64 / width) * 65535.0) as u32;
    let y = ((point.y as f64 / height) * 65535.0) as u32;

    let move_input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: x as i32,
                dy: y as i32,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let down_input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_LEFTDOWN,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let up_input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_LEFTUP,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [move_input, down_input, up_input];
    let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    if sent != inputs.len() as u32 {
        Err("SendInput 鼠标注入失败".to_string())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn send_unicode(text: &str) -> Result<(), String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
        VIRTUAL_KEY,
    };
    let mut inputs = Vec::new();
    for ch in text.chars() {
        let scan = ch as u16;
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: scan,
                    dwFlags: KEYEVENTF_UNICODE,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: scan,
                    dwFlags: KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
    }
    if inputs.is_empty() {
        return Ok(());
    }
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent == inputs.len() as u32 {
        Ok(())
    } else {
        Err("SendInput 文本注入失败".to_string())
    }
}

#[cfg(target_os = "windows")]
fn send_shortcut(combo: &str) -> Result<(), String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VIRTUAL_KEY,
    };
    let (modifiers, key) = parse_shortcut(combo)?;
    let mut inputs = Vec::new();
    for vk in &modifiers {
        inputs.push(key_input(VIRTUAL_KEY(*vk), KEYBD_EVENT_FLAGS(0)));
    }
    inputs.push(key_input(VIRTUAL_KEY(key), KEYBD_EVENT_FLAGS(0)));
    inputs.push(key_input(VIRTUAL_KEY(key), KEYEVENTF_KEYUP));
    for vk in modifiers.iter().rev() {
        inputs.push(key_input(VIRTUAL_KEY(*vk), KEYEVENTF_KEYUP));
    }
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent == inputs.len() as u32 {
        Ok(())
    } else {
        Err("SendInput 快捷键注入失败".to_string())
    }
}

/// 主动启动应用或打开 URL（不依赖应用已在前台）。
#[cfg(target_os = "windows")]
fn launch_target(target: &str) -> Result<(), String> {
    if target.trim().is_empty() {
        return Err("启动目标为空".to_string());
    }
    let quoted = format!("\"{}\"", target.replace('"', ""));
    std::process::Command::new("cmd")
        .args(["/C", "start", "", &quoted])
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("启动失败：{error}"))
}

fn parse_click_at(text: &str) -> Result<(i32, i32), String> {
    let mut parts = text.split(',');
    let x = parts
        .next()
        .ok_or_else(|| "点击坐标缺少 x".to_string())?
        .trim()
        .parse::<i32>()
        .map_err(|_| format!("x 坐标非法：{text}"))?;
    let y = parts
        .next()
        .ok_or_else(|| "点击坐标缺少 y".to_string())?
        .trim()
        .parse::<i32>()
        .map_err(|_| format!("y 坐标非法：{text}"))?;
    Ok((x, y))
}

#[cfg(target_os = "windows")]
fn click_at_screen(x: i32, y: i32) -> Result<(), String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
        MOUSEINPUT,
    };
    use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;
    unsafe { SetCursorPos(x, y) }.map_err(|error| format!("SetCursorPos 失败：{error}"))?;
    let mouse = |flags| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [mouse(MOUSEEVENTF_LEFTDOWN), mouse(MOUSEEVENTF_LEFTUP)];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent == inputs.len() as u32 {
        Ok(())
    } else {
        Err("SendInput 鼠标点击失败".to_string())
    }
}

#[cfg(not(target_os = "windows"))]
fn launch_target(target: &str) -> Result<(), String> {
    if target.trim().is_empty() {
        return Err("启动目标为空".to_string());
    }
    let command = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(command)
        .arg(target)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("启动失败：{error}"))
}

#[cfg(target_os = "windows")]
fn key_input(
    vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY,
    flags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS,
) -> windows::Win32::UI::Input::KeyboardAndMouse::INPUT {
    use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT};
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

#[cfg(target_os = "windows")]
fn parse_shortcut(combo: &str) -> Result<(Vec<u16>, u16), String> {
    const VK_CONTROL: u16 = 0x11;
    const VK_MENU: u16 = 0x12;
    const VK_SHIFT: u16 = 0x10;
    const VK_LWIN: u16 = 0x5B;
    let mut modifiers = Vec::new();
    let mut key = None;
    for part in combo.split('+') {
        let part = part.trim().to_lowercase();
        match part.as_str() {
            "ctrl" | "control" => modifiers.push(VK_CONTROL),
            "alt" => modifiers.push(VK_MENU),
            "shift" => modifiers.push(VK_SHIFT),
            "win" | "cmd" => modifiers.push(VK_LWIN),
            "enter" => key = Some(0x0D),
            "tab" => key = Some(0x09),
            "esc" | "escape" => key = Some(0x1B),
            "space" => key = Some(0x20),
            "backspace" => key = Some(0x08),
            "delete" | "del" => key = Some(0x2E),
            "up" => key = Some(0x26),
            "down" => key = Some(0x28),
            "left" => key = Some(0x25),
            "right" => key = Some(0x27),
            _ => {
                if let Some(letter) = part.chars().next() {
                    if letter.is_ascii_alphanumeric() {
                        key = Some(letter.to_ascii_uppercase() as u16);
                    } else if part.starts_with('f') && part.len() > 1 {
                        if let Ok(number) = part[1..].parse::<u16>() {
                            if (1..=24).contains(&number) {
                                key = Some(0x70 + number - 1);
                            }
                        }
                    }
                }
                if key.is_none() {
                    return Err(format!("无法解析快捷键：{combo}"));
                }
            }
        }
    }
    let key = key.ok_or_else(|| format!("快捷键缺少按键：{combo}"))?;
    Ok((modifiers, key))
}

#[cfg(not(target_os = "windows"))]
pub struct WindowsUiaSource;

#[cfg(not(target_os = "windows"))]
impl WindowsUiaSource {
    pub fn new() -> Result<Self, String> {
        Err("仅支持 Windows".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learn::{ActionGraph, ActionType, SemanticAnchor};

    struct ScriptedSource {
        find_ok: bool,
        invoke_ok: bool,
        verify_ok: bool,
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl UiActionSource for ScriptedSource {
        fn find(&self, anchor: &SemanticAnchor) -> Result<u64, String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("find:{}", anchor.name));
            if self.find_ok {
                Ok(1)
            } else {
                Err(format!("未找到：{}", anchor.name))
            }
        }
        fn invoke(&self, _handle: u64) -> Result<(), String> {
            self.calls.lock().unwrap().push("invoke".to_string());
            if self.invoke_ok {
                Ok(())
            } else {
                Err("调用失败".to_string())
            }
        }
        fn type_text(&self, _handle: u64, text: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("type:{text}"));
            Ok(())
        }
        fn shortcut(&self, combo: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("shortcut:{combo}"));
            Ok(())
        }
        fn launch(&self, target: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("launch:{target}"));
            if target.trim().is_empty() {
                Err("启动目标为空".to_string())
            } else {
                Ok(())
            }
        }
        fn click_at(&self, x: i32, y: i32) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("click_at:{x},{y}"));
            Ok(())
        }
        fn verify(&self, predicate: &str) -> Result<bool, String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("verify:{predicate}"));
            Ok(self.verify_ok)
        }
    }

    fn graph() -> ActionGraph {
        let mut graph = ActionGraph::new();
        graph.add_node(
            "find",
            ActionType::Click,
            SemanticAnchor {
                app_id: Some("qq".to_string()),
                role: Some("button".to_string()),
                name: "发送按钮".to_string(),
                parent: None,
            },
            None,
            Some("发送成功".to_string()),
        );
        graph.add_node(
            "type",
            ActionType::Type,
            SemanticAnchor {
                app_id: Some("qq".to_string()),
                role: Some("edit".to_string()),
                name: "输入框".to_string(),
                parent: None,
            },
            Some("你好 {contact}".to_string()),
            None,
        );
        graph.add_edge("find", "type", None, None);
        graph
    }

    #[test]
    fn executes_linear_graph_with_variables_and_verification() {
        let source = ScriptedSource {
            find_ok: true,
            invoke_ok: true,
            verify_ok: true,
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let variables = HashMap::from([("contact".to_string(), "小李".to_string())]);
        let report = execute_graph(&source, &graph(), &variables, 10);
        assert!(report.ok, "{report:?}");
        assert_eq!(report.steps.len(), 2);
        let calls = source.calls.lock().unwrap().clone();
        assert!(calls.iter().any(|call| call == "type:你好 小李"));
        assert!(calls.iter().any(|call| call == "verify:发送成功"));
    }

    #[test]
    fn launch_action_invokes_source_and_validates_target() {
        let mut graph = ActionGraph::new();
        graph.add_node(
            "open",
            ActionType::Launch,
            SemanticAnchor {
                app_id: None,
                role: None,
                name: "打开浏览器".to_string(),
                parent: None,
            },
            Some("{url}".to_string()),
            None,
        );
        let source = ScriptedSource {
            find_ok: true,
            invoke_ok: true,
            verify_ok: true,
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let report = execute_graph(
            &source,
            &graph,
            &HashMap::from([("url".to_string(), "https://example.com".to_string())]),
            10,
        );
        assert!(report.ok, "{report:?}");
        assert!(source
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "launch:https://example.com"));
        let report = execute_graph(
            &source,
            &graph,
            &HashMap::from([("url".to_string(), String::new())]),
            10,
        );
        assert!(!report.ok);
    }

    #[test]
    fn click_at_action_parses_coordinates_and_invokes_source() {
        let mut graph = ActionGraph::new();
        graph.add_node(
            "click",
            ActionType::ClickAt,
            SemanticAnchor {
                app_id: None,
                role: None,
                name: "OCR 定位按钮".to_string(),
                parent: None,
            },
            Some("{point}".to_string()),
            None,
        );
        let source = ScriptedSource {
            find_ok: true,
            invoke_ok: true,
            verify_ok: true,
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let report = execute_graph(
            &source,
            &graph,
            &HashMap::from([("point".to_string(), "123,456".to_string())]),
            10,
        );
        assert!(report.ok, "{report:?}");
        assert!(source
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "click_at:123,456"));
        let report = execute_graph(
            &source,
            &graph,
            &HashMap::from([("point".to_string(), "abc".to_string())]),
            10,
        );
        assert!(!report.ok);
    }

    #[test]
    fn parse_click_at_rejects_invalid_input() {
        assert_eq!(parse_click_at("10,20").unwrap(), (10, 20));
        assert!(parse_click_at("10").is_err());
        assert!(parse_click_at("a,b").is_err());
        assert!(parse_click_at("").is_err());
    }

    #[test]
    fn reports_failure_when_invoke_fails() {
        let source = ScriptedSource {
            find_ok: true,
            invoke_ok: false,
            verify_ok: true,
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let report = execute_graph(&source, &graph(), &HashMap::new(), 10);
        assert!(!report.ok);
        assert_eq!(report.steps[0].status, "failed");
    }

    #[test]
    fn blocks_sensitive_anchors_without_calling_source() {
        let mut graph = ActionGraph::new();
        graph.add_node(
            "password",
            ActionType::Type,
            SemanticAnchor {
                app_id: None,
                role: None,
                name: "密码输入框".to_string(),
                parent: None,
            },
            None,
            None,
        );
        let source = ScriptedSource {
            find_ok: true,
            invoke_ok: true,
            verify_ok: true,
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let report = execute_graph(&source, &graph, &HashMap::new(), 10);
        assert!(!report.ok);
        assert_eq!(report.steps[0].status, "blocked");
        assert!(source.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn detects_cycles() {
        let mut graph = ActionGraph::new();
        graph.add_node(
            "a",
            ActionType::Click,
            SemanticAnchor {
                app_id: None,
                role: None,
                name: "A".to_string(),
                parent: None,
            },
            None,
            None,
        );
        graph.add_edge("a", "a", None, None);
        let source = ScriptedSource {
            find_ok: true,
            invoke_ok: true,
            verify_ok: true,
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let report = execute_graph(&source, &graph, &HashMap::new(), 10);
        assert!(!report.ok);
        assert!(report.error.unwrap().contains("成环"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn empty_element_name_does_not_match_any_anchor() {
        let anchor = SemanticAnchor {
            app_id: None,
            role: None,
            name: "发送".to_string(),
            parent: None,
        };
        assert!(!anchor_matches(&anchor, "", 0));
        assert!(anchor_matches(&anchor, "发送按钮", 0));
    }
}
