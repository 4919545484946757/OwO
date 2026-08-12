//! 平台事件源（v0.4 P2，L0 事件层）。
//!
//! 当前实现：Windows 前台窗口（app_id + 标题）。其余事件源
//! （剪贴板/无障碍 UI 树/截图）在后续迭代接入。

/// 轮询当前前台应用，返回 `(app_id, 窗口标题)`。
/// 非 Windows 平台暂不采集。
#[cfg(target_os = "windows")]
pub fn poll_foreground_app() -> Option<(String, String)> {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
    };

    unsafe {
        let hwnd: HWND = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let length = GetWindowTextLengthW(hwnd);
        if length <= 0 {
            return None;
        }
        let mut buffer = vec![0u16; (length + 1) as usize];
        let written = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
        let title = String::from_utf16_lossy(&buffer[..written.max(0) as usize])
            .trim()
            .to_string();

        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let app_id = process_name(pid).unwrap_or_else(|| format!("pid:{pid}"));
        Some((app_id, title))
    }
}

#[cfg(target_os = "windows")]
fn process_name(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buffer = [0u16; 1024];
        let mut size = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buffer[..size as usize]);
        path.rsplit('\\')
            .next()
            .map(|name| name.trim_end_matches(".exe").to_lowercase())
    }
}

#[cfg(not(target_os = "windows"))]
pub fn poll_foreground_app() -> Option<(String, String)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreground_poll_is_callable() {
        // Windows 交互会话返回真实前台窗口；无窗口时不返回数据。
        let _ = poll_foreground_app();
    }
}
