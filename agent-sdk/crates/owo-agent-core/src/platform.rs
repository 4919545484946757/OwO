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
pub(crate) fn process_name(pid: u32) -> Option<String> {
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

/// 当前前台窗口标题（执行引擎验证用）。
#[cfg(target_os = "windows")]
pub fn foreground_title() -> Option<String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW,
    };
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let length = GetWindowTextLengthW(hwnd);
        if length <= 0 {
            return None;
        }
        let mut buffer = vec![0u16; (length + 1) as usize];
        let written = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
        Some(
            String::from_utf16_lossy(&buffer[..written.max(0) as usize])
                .trim()
                .to_string(),
        )
    }
}

#[cfg(not(target_os = "windows"))]
pub fn foreground_title() -> Option<String> {
    None
}

/// 当前前台窗口的屏幕矩形 `(left, top, right, bottom)`。
#[cfg(target_os = "windows")]
pub fn foreground_window_rect() -> Option<(i32, i32, i32, i32)> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowRect};
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let mut rect: windows_sys::Win32::Foundation::RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut rect) == 0 {
            return None;
        }
        Some((rect.left, rect.top, rect.right, rect.bottom))
    }
}

#[cfg(not(target_os = "windows"))]
pub fn foreground_window_rect() -> Option<(i32, i32, i32, i32)> {
    None
}

/// 窗口摘要（枚举用）。
#[cfg(target_os = "windows")]
#[derive(Debug, Clone, serde::Serialize)]
pub struct WindowInfo {
    pub hwnd: isize,
    pub pid: u32,
    pub process: String,
    pub title: String,
    pub rect: (i32, i32, i32, i32),
    pub visible: bool,
}

/// 窗口截图结果：BMP 字节 + 窗口屏幕矩形 `(left, top, right, bottom)`。
pub type WindowBmp = (Vec<u8>, (i32, i32, i32, i32));

/// 枚举顶层窗口（进程名 + 标题 + 屏幕矩形），供“找到 QQ/浏览器窗口并激活”使用。
#[cfg(target_os = "windows")]
pub fn window_list() -> Vec<WindowInfo> {
    use windows_sys::Win32::Foundation::{BOOL, HWND};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
        IsWindowVisible,
    };
    let mut out: Vec<WindowInfo> = Vec::new();
    unsafe extern "system" fn callback(hwnd: HWND, param: isize) -> BOOL {
        let list = &mut *(param as *mut Vec<WindowInfo>);
        let length = unsafe { GetWindowTextLengthW(hwnd) };
        if length > 0 {
            let mut buffer = vec![0u16; (length + 1) as usize];
            let written = unsafe { GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
            let title = String::from_utf16_lossy(&buffer[..written.max(0) as usize])
                .trim()
                .to_string();
            let mut pid: u32 = 0;
            unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
            let mut rect: windows_sys::Win32::Foundation::RECT = std::mem::zeroed();
            let rect_ok = unsafe { GetWindowRect(hwnd, &mut rect) } != 0;
            let visible = unsafe { IsWindowVisible(hwnd) } != 0;
            let process =
                crate::platform::process_name(pid).unwrap_or_else(|| format!("pid:{pid}"));
            list.push(WindowInfo {
                hwnd: hwnd as isize,
                pid,
                process,
                title,
                rect: if rect_ok {
                    (rect.left, rect.top, rect.right, rect.bottom)
                } else {
                    (0, 0, 0, 0)
                },
                visible,
            });
        }
        1
    }
    unsafe {
        EnumWindows(Some(callback), &mut out as *mut Vec<WindowInfo> as isize);
    }
    out
}

/// 后台只读抓取指定窗口内容（PrintWindow，可抓被遮挡窗口），返回 BMP 与屏幕矩形。
#[cfg(target_os = "windows")]
pub fn capture_window_bmp(hwnd: isize) -> Option<WindowBmp> {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        HGDIOBJ, SRCCOPY,
    };
    use windows_sys::Win32::Storage::Xps::PrintWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowRect, PW_RENDERFULLCONTENT};
    unsafe {
        let hwnd = hwnd as windows_sys::Win32::Foundation::HWND;
        let mut rect: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut rect) == 0 {
            return None;
        }
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width <= 0 || height <= 0 {
            return None;
        }
        let window_dc = GetDC(hwnd);
        if window_dc.is_null() {
            return None;
        }
        let memory = CreateCompatibleDC(window_dc);
        let bitmap = CreateCompatibleBitmap(window_dc, width, height);
        if memory.is_null() || bitmap.is_null() {
            let _ = ReleaseDC(hwnd, window_dc);
            if !memory.is_null() {
                DeleteDC(memory);
            }
            if !bitmap.is_null() {
                DeleteObject(bitmap as HGDIOBJ);
            }
            return None;
        }
        let old = SelectObject(memory, bitmap as HGDIOBJ);
        let printed = PrintWindow(hwnd, memory, PW_RENDERFULLCONTENT);
        if printed == 0 {
            BitBlt(memory, 0, 0, width, height, window_dc, 0, 0, SRCCOPY);
        }
        SelectObject(memory, old);
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = width;
        info.bmiHeader.biHeight = -height;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        let mut pixels = vec![0u8; (width as usize) * (height as usize) * 4];
        let got = GetDIBits(
            memory,
            bitmap,
            0,
            height as u32,
            pixels.as_mut_ptr() as *mut core::ffi::c_void,
            &mut info,
            DIB_RGB_COLORS,
        );
        let _ = DeleteObject(bitmap as HGDIOBJ);
        let _ = DeleteDC(memory);
        let _ = ReleaseDC(hwnd, window_dc);
        if got == 0 {
            return None;
        }
        Some((
            encode_bmp(width, height, &pixels),
            (rect.left, rect.top, rect.right, rect.bottom),
        ))
    }
}

#[cfg(not(target_os = "windows"))]
pub fn capture_window_bmp(_hwnd: isize) -> Option<WindowBmp> {
    None
}

#[cfg(not(target_os = "windows"))]
pub fn window_list() -> Vec<WindowInfo> {
    Vec::new()
}

#[cfg(not(target_os = "windows"))]
#[derive(Debug, Clone, serde::Serialize)]
pub struct WindowInfo {
    pub hwnd: isize,
    pub pid: u32,
    pub process: String,
    pub title: String,
    pub rect: (i32, i32, i32, i32),
    pub visible: bool,
}

/// 激活目标窗口（按进程名或标题模糊匹配）：还原最小化 → SetForegroundWindow。
#[cfg(target_os = "windows")]
pub fn activate_window(process: &str, title: &str) -> Result<(), String> {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::Threading::AttachThreadInput;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        keybd_event, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, VK_MENU,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, IsIconic,
        SetForegroundWindow, SetWindowPos, ShowWindow, HWND_NOTOPMOST, HWND_TOP, HWND_TOPMOST,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_RESTORE,
    };
    let candidates = window_list();
    let target = candidates
        .iter()
        .find(|window| {
            window.visible
                && ((!process.is_empty() && window.process.contains(process))
                    || (!title.is_empty() && window.title.contains(title)))
        })
        .ok_or_else(|| format!("未找到窗口（process={process}, title={title}）"))?;
    let hwnd: HWND = target.hwnd as HWND;
    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        let foreground = GetForegroundWindow();
        let mut foreground_thread = 0;
        GetWindowThreadProcessId(foreground, &mut foreground_thread);
        let mut target_thread = 0;
        GetWindowThreadProcessId(hwnd, &mut target_thread);
        if foreground_thread != 0 && target_thread != 0 {
            AttachThreadInput(foreground_thread, target_thread, 1);
        }
        if foreground != hwnd {
            // 经典解锁：发送一次 Alt 键事件，满足 Windows 前台锁的“最近用户输入”条件。
            keybd_event(VK_MENU as u8, 0, KEYEVENTF_EXTENDEDKEY, 0);
            keybd_event(VK_MENU as u8, 0, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP, 0);
            SetForegroundWindow(hwnd);
            BringWindowToTop(hwnd);
            SetWindowPos(
                hwnd,
                HWND_TOP,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            // 临时置顶再取消：把窗口真正抬到非置顶窗口之上（应对被 Codex/浏览器遮挡）。
            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            );
            SetWindowPos(
                hwnd,
                HWND_NOTOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            );
        } else {
            SetForegroundWindow(hwnd);
        }
        if foreground_thread != 0 && target_thread != 0 {
            AttachThreadInput(foreground_thread, target_thread, 0);
        }
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub fn activate_window(process: &str, title: &str) -> Result<(), String> {
    Err(format!("平台不支持激活窗口（{process}/{title}）"))
}

/// 当前剪贴板序列号（L0 事件源，只做“是否变化”检测，不读取内容）。
#[cfg(target_os = "windows")]
pub fn clipboard_sequence() -> u32 {
    unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() }
}

#[cfg(not(target_os = "windows"))]
pub fn clipboard_sequence() -> u32 {
    0
}

/// 按需截图（L2）：返回内存中的 BMP 字节，不落盘。
#[cfg(target_os = "windows")]
pub fn capture_screen() -> Option<Vec<u8>> {
    let (width, height) = screen_size()?;
    capture_screen_region(width, height)
}

#[cfg(not(target_os = "windows"))]
pub fn capture_screen() -> Option<Vec<u8>> {
    None
}

/// 截取屏幕指定区域（测试与按需采集用），返回内存 BMP。
#[cfg(target_os = "windows")]
pub fn capture_screen_region(width: i32, height: i32) -> Option<Vec<u8>> {
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        HGDIOBJ, SRCCOPY,
    };

    if width <= 0 || height <= 0 {
        return None;
    }
    unsafe {
        let screen = GetDC(std::ptr::null_mut());
        if screen.is_null() {
            return None;
        }
        let memory = CreateCompatibleDC(screen);
        if memory.is_null() {
            ReleaseDC(std::ptr::null_mut(), screen);
            return None;
        }
        let bitmap = CreateCompatibleBitmap(screen, width, height);
        if bitmap.is_null() {
            DeleteDC(memory);
            ReleaseDC(std::ptr::null_mut(), screen);
            return None;
        }
        SelectObject(memory, bitmap as HGDIOBJ);
        let copied = BitBlt(memory, 0, 0, width, height, screen, 0, 0, SRCCOPY);

        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = width;
        info.bmiHeader.biHeight = -height; // top-down
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        let mut pixels = vec![0u8; (width as usize) * (height as usize) * 4];
        let got = if copied != 0 {
            GetDIBits(
                memory,
                bitmap,
                0,
                height as u32,
                pixels.as_mut_ptr() as *mut core::ffi::c_void,
                &mut info,
                DIB_RGB_COLORS,
            )
        } else {
            0
        };
        DeleteObject(bitmap as HGDIOBJ);
        DeleteDC(memory);
        ReleaseDC(std::ptr::null_mut(), screen);
        if got == 0 {
            return None;
        }
        Some(encode_bmp(width, height, &pixels))
    }
}

#[cfg(not(target_os = "windows"))]
pub fn capture_screen_region(_width: i32, _height: i32) -> Option<Vec<u8>> {
    None
}

#[cfg(target_os = "windows")]
fn screen_size() -> Option<(i32, i32)> {
    use windows_sys::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, ReleaseDC, HORZRES, VERTRES};
    unsafe {
        let screen = GetDC(std::ptr::null_mut());
        if screen.is_null() {
            return None;
        }
        let width = GetDeviceCaps(screen, HORZRES as i32);
        let height = GetDeviceCaps(screen, VERTRES as i32);
        ReleaseDC(std::ptr::null_mut(), screen);
        if width <= 0 || height <= 0 {
            None
        } else {
            Some((width, height))
        }
    }
}

/// 内存 BMP 封装（14 字节文件头 + 40 字节 DIB 头 + BGRA 像素）。
fn encode_bmp(width: i32, height: i32, bgra: &[u8]) -> Vec<u8> {
    let pixel_bytes = bgra.len();
    let file_size = 54 + pixel_bytes;
    let mut out = Vec::with_capacity(file_size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(file_size as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&(pixel_bytes as u32).to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(bgra);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreground_poll_is_callable() {
        // Windows 交互会话返回真实前台窗口；无窗口时不返回数据。
        let _ = poll_foreground_app();
    }

    #[test]
    fn clipboard_sequence_is_callable() {
        let _ = clipboard_sequence();
    }

    #[test]
    fn screen_capture_region_produces_bmp() {
        // 4x4 采样验证 GDI 链路；无窗口会话可能返回 None（不做强断言）。
        if let Some(bmp) = capture_screen_region(4, 4) {
            assert_eq!(&bmp[..2], b"BM");
            assert!(bmp.len() > 54);
        }
    }
}
