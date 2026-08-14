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
        // D3D/Chromium 窗口 PrintWindow 偶发只捕获部分内容：重试几次。
        let mut printed = 0;
        for _attempt in 0..3 {
            printed = PrintWindow(hwnd, memory, PW_RENDERFULLCONTENT);
            if printed != 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
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
        // D3D/Chromium 窗口 PrintWindow 可能缺底部：再用 BitBlt 抓一次，选内容更丰富的一帧。
        let mut bitblt_pixels = vec![0u8; (width as usize) * (height as usize) * 4];
        let bitblt_ok = BitBlt(memory, 0, 0, width, height, window_dc, 0, 0, SRCCOPY) != 0;
        if bitblt_ok {
            GetDIBits(
                memory,
                bitmap,
                0,
                height as u32,
                bitblt_pixels.as_mut_ptr() as *mut core::ffi::c_void,
                &mut info,
                DIB_RGB_COLORS,
            );
        }
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
        if bitblt_ok && bmp_richness(&bitblt_pixels) > bmp_richness(&pixels) * 1.1 {
            pixels = bitblt_pixels;
        }
        Some((
            encode_bmp(width, height, &pixels),
            (rect.left, rect.top, rect.right, rect.bottom),
        ))
    }
}

/// 深度窗口抓取：主窗口 + 所有子窗口逐帧 PrintWindow，选内容最丰富的一帧。
/// Chromium/D3D 应用（如 QQ）主窗口 DC 不含渲染子窗口内容时使用。
#[cfg(target_os = "windows")]
pub fn capture_window_bmp_deep(hwnd: isize) -> Option<WindowBmp> {
    use windows_sys::Win32::Foundation::{BOOL, HWND};
    use windows_sys::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GetWindowRect};
    let mut candidates: Vec<isize> = vec![hwnd];
    unsafe extern "system" fn collect_child(hwnd: HWND, param: isize) -> BOOL {
        let list = &mut *(param as *mut Vec<isize>);
        list.push(hwnd as isize);
        1
    }
    unsafe {
        EnumChildWindows(
            hwnd as HWND,
            Some(collect_child),
            &mut candidates as *mut Vec<isize> as isize,
        );
    }
    let mut best: Option<(f64, WindowBmp)> = None;
    for candidate in candidates {
        let mut rect: windows_sys::Win32::Foundation::RECT = unsafe { std::mem::zeroed() };
        let size_ok = unsafe { GetWindowRect(candidate as HWND, &mut rect) } != 0
            && (rect.right - rect.left) * (rect.bottom - rect.top) >= 40_000;
        if !size_ok {
            continue;
        }
        let Some(capture) = capture_window_bmp(candidate) else {
            continue;
        };
        let (bmp, _rect) = &capture;
        let richness = bmp_richness(bmp);
        if best
            .as_ref()
            .map(|(best_rich, _)| richness > *best_rich)
            .unwrap_or(true)
        {
            best = Some((richness, capture));
        }
    }
    best.map(|(_, capture)| capture)
}

#[cfg(not(target_os = "windows"))]
pub fn capture_window_bmp_deep(_hwnd: isize) -> Option<WindowBmp> {
    None
}

/// 内容丰富度：非纯白/纯黑像素占比（用于 D3D 抓帧择优）。
#[cfg(target_os = "windows")]
fn bmp_richness(bgra: &[u8]) -> f64 {
    let total = (bgra.len() / 4).max(1);
    let mut rich = 0usize;
    for pixel in bgra.chunks_exact(4) {
        let (b, g, r) = (pixel[0] as i32, pixel[1] as i32, pixel[2] as i32);
        let sum = r + g + b;
        let spread = r.max(g).max(b) - r.min(g).min(b);
        if spread > 24 || sum < 690 {
            rich += 1;
        }
    }
    rich as f64 / total as f64
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
            let activated = SetForegroundWindow(hwnd) != 0;
            if !activated {
                // 经典解锁：仅在激活失败时发送 Alt 键，避免破坏输入队列。
                keybd_event(VK_MENU as u8, 0, KEYEVENTF_EXTENDEDKEY, 0);
                keybd_event(VK_MENU as u8, 0, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP, 0);
                std::thread::sleep(std::time::Duration::from_millis(120));
                SetForegroundWindow(hwnd);
            }
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

/// 用 GDI 在内存位图上渲染文本（白底黑字，支持 `\n` 换行），返回 32bpp BMP。
/// 纯软件渲染（CreateDIBSection + TextOutW），不依赖交互桌面/前台窗口，
/// 供 ONNX OCR 集成测试生成已知文本的样本图。
#[cfg(target_os = "windows")]
pub fn render_text_bmp(text: &str, font_size: i32) -> Option<Vec<u8>> {
    use windows_sys::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject,
        GetTextExtentPoint32W, SelectObject, SetBkColor, SetBkMode, SetTextColor, TextOutW,
        BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
        DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, FW_NORMAL, HDC, HGDIOBJ,
        OUT_DEFAULT_PRECIS, TRANSPARENT,
    };
    unsafe {
        let lines: Vec<Vec<u16>> = text
            .split('\n')
            .map(|line| line.encode_utf16().collect())
            .collect();
        if lines.iter().all(|l| l.is_empty()) {
            return None;
        }
        let font = CreateFontW(
            -font_size,
            0,
            0,
            0,
            FW_NORMAL as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            DEFAULT_PITCH as u32,
            "Microsoft YaHei\0"
                .encode_utf16()
                .collect::<Vec<u16>>()
                .as_ptr(),
        );
        if font.is_null() {
            return None;
        }
        let memory: HDC = CreateCompatibleDC(std::ptr::null_mut());
        if memory.is_null() {
            DeleteObject(font as HGDIOBJ);
            return None;
        }
        let old_font = SelectObject(memory, font as HGDIOBJ);
        let mut extents = Vec::with_capacity(lines.len());
        let mut max_width = 0i32;
        for line in &lines {
            let mut extent: windows_sys::Win32::Foundation::SIZE = std::mem::zeroed();
            if GetTextExtentPoint32W(memory, line.as_ptr(), line.len() as i32, &mut extent) == 0 {
                SelectObject(memory, old_font);
                DeleteObject(font as HGDIOBJ);
                DeleteDC(memory);
                return None;
            }
            max_width = max_width.max(extent.cx);
            extents.push(extent);
        }
        let margin = 8i32;
        let line_height = if extents.is_empty() {
            font_size
        } else {
            extents[0].cy
        };
        let width = max_width + margin * 2;
        let height = (extents.len() as i32) * line_height + margin * 2;
        if width <= 0 || height <= 0 {
            SelectObject(memory, old_font);
            DeleteObject(font as HGDIOBJ);
            DeleteDC(memory);
            return None;
        }
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = width;
        info.bmiHeader.biHeight = -height; // top-down
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(
            std::ptr::null_mut(),
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );
        if dib.is_null() || bits.is_null() {
            SelectObject(memory, old_font);
            DeleteObject(font as HGDIOBJ);
            DeleteDC(memory);
            return None;
        }
        let old_bitmap = SelectObject(memory, dib as HGDIOBJ);
        // 白底
        let pixel_bytes = (width as usize) * (height as usize) * 4;
        std::ptr::write_bytes(bits as *mut u8, 0xFF, pixel_bytes);
        // 黑字（逐行）
        SetBkMode(memory, TRANSPARENT as i32);
        SetBkColor(memory, 0x00FFFFFF);
        SetTextColor(memory, 0x00000000);
        for (index, line) in lines.iter().enumerate() {
            if !line.is_empty() {
                TextOutW(
                    memory,
                    margin,
                    margin + (index as i32) * line_height,
                    line.as_ptr(),
                    line.len() as i32,
                );
            }
        }
        let bgra = std::slice::from_raw_parts(bits as *const u8, pixel_bytes).to_vec();
        SelectObject(memory, old_bitmap);
        SelectObject(memory, old_font);
        DeleteObject(dib as HGDIOBJ);
        DeleteObject(font as HGDIOBJ);
        DeleteDC(memory);
        Some(encode_bmp(width, height, &bgra))
    }
}

#[cfg(not(target_os = "windows"))]
pub fn render_text_bmp(_text: &str, _font_size: i32) -> Option<Vec<u8>> {
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

    #[test]
    fn render_text_bmp_produces_32bpp_bmp() {
        // 文本渲染是纯内存 GDI，任何会话都应可用。
        let bmp = render_text_bmp("OwO 测试", 32).expect("GDI 文本渲染应可用");
        assert_eq!(&bmp[..2], b"BM");
        let bit_count = u16::from_le_bytes([bmp[28], bmp[29]]);
        assert_eq!(bit_count, 32);
        let width = i32::from_le_bytes([bmp[18], bmp[19], bmp[20], bmp[21]]);
        let height = i32::from_le_bytes([bmp[22], bmp[23], bmp[24], bmp[25]]).abs();
        assert!(width > 0 && height > 0);
        // 白底上应有非白像素（文字痕迹）
        let pixels = &bmp[54..];
        let non_white = pixels
            .chunks_exact(4)
            .filter(|px| px[0] != 0xFF || px[1] != 0xFF || px[2] != 0xFF)
            .count();
        assert!(non_white > 0, "渲染结果应包含非白像素（文字）");
    }
}
