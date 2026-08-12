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
