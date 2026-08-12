//! L1 界面层（v0.4 P2）：Windows UI Automation 无障碍 UI 树摘要。
//!
//! 只取语义锚点（角色 + 名称 + 类名），不包含坐标序列；供情景模型与操作学习使用。
use serde::{Deserialize, Serialize};

#[cfg(target_os = "windows")]
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    COINIT_DISABLE_OLE1DDE,
};
#[cfg(target_os = "windows")]
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, TreeScope_Children,
};
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiNode {
    pub name: String,
    pub control_type: i32,
    pub class: String,
    pub depth: u32,
    /// 元素在屏幕上的边界框（OCR/坐标点击定位用）。
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default)]
    pub width: i32,
    #[serde(default)]
    pub height: i32,
}

/// 抓取前台窗口的无障碍 UI 树摘要（Windows UI Automation）。
#[cfg(target_os = "windows")]
pub fn foreground_ui_tree(max_depth: u32, max_nodes: usize) -> Option<Vec<UiNode>> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return None;
    }
    ui_tree_for_hwnd(hwnd.0 as isize, max_depth, max_nodes)
}

/// 按窗口句柄抓取无障碍 UI 树（不要求前台，窗口存在即可，用于窗口模板/后台情景理解）。
#[cfg(target_os = "windows")]
pub fn ui_tree_for_hwnd(hwnd: isize, max_depth: u32, max_nodes: usize) -> Option<Vec<UiNode>> {
    unsafe {
        // COM 已在 MTA 时忽略错误，UIA 仍可工作。
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let automation: IUIAutomation =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
        let root = automation
            .ElementFromHandle(windows::Win32::Foundation::HWND(
                hwnd as *mut core::ffi::c_void,
            ))
            .ok()?;
        let mut nodes = Vec::new();
        collect_ui(&automation, &root, 0, max_depth, max_nodes, &mut nodes);
        if nodes.is_empty() {
            None
        } else {
            Some(nodes)
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub fn ui_tree_for_hwnd(_hwnd: isize, _max_depth: u32, _max_nodes: usize) -> Option<Vec<UiNode>> {
    None
}

#[cfg(target_os = "windows")]
fn collect_ui(
    automation: &IUIAutomation,
    element: &IUIAutomationElement,
    depth: u32,
    max_depth: u32,
    max_nodes: usize,
    out: &mut Vec<UiNode>,
) {
    unsafe {
        if out.len() >= max_nodes {
            return;
        }
        let name = element
            .CurrentName()
            .map(|value| value.to_string())
            .unwrap_or_default();
        let class = element
            .CurrentClassName()
            .map(|value| value.to_string())
            .unwrap_or_default();
        let control_type = element
            .CurrentControlType()
            .map(|value| value.0)
            .unwrap_or(0);
        let rect = element
            .CurrentBoundingRectangle()
            .map(|rect| {
                (
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                )
            })
            .unwrap_or((0, 0, 0, 0));
        if !name.trim().is_empty() || !class.trim().is_empty() {
            out.push(UiNode {
                name,
                control_type,
                class,
                depth,
                x: rect.0,
                y: rect.1,
                width: rect.2,
                height: rect.3,
            });
        }
        if depth >= max_depth {
            return;
        }
        let Ok(condition) = automation.CreateTrueCondition() else {
            return;
        };
        let Ok(children) = element.FindAll(TreeScope_Children, &condition) else {
            return;
        };
        let Ok(length) = children.Length() else {
            return;
        };
        for index in 0..length {
            if out.len() >= max_nodes {
                return;
            }
            if let Ok(child) = children.GetElement(index) {
                collect_ui(automation, &child, depth + 1, max_depth, max_nodes, out);
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub fn foreground_ui_tree(_max_depth: u32, _max_nodes: usize) -> Option<Vec<UiNode>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_tree_poll_is_callable() {
        // 交互会话返回前台应用的无障碍树；无窗口/不可用返回 None（不强制断言）。
        let _ = foreground_ui_tree(2, 32);
    }
}
