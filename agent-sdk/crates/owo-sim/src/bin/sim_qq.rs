//! OwO 模拟 QQ（桌面实验台）：GDI 自绘聊天窗口，无 UIA 控件文本。
//!
//! 用途：在后台快速验证“OCR 读取上下文 → 输入 → 点击发送 → OCR 验证 →
//! 等待对方回复 → 再回复”的完整桌面 Agent 闭环，避免反复打扰真实 QQ。
//!
//! 用法：owo-sim-qq --scenario <json> --log <jsonl>
//! 所有事件（incoming/outgoing/点击）写入 JSONL 日志，作为测试真值。

#![cfg(target_os = "windows")]
#![windows_subsystem = "windows"]

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::Local;
use serde::Deserialize;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreateSolidBrush,
    DeleteDC, DeleteObject, EndPaint, FillRect, GetDC, GetDIBits, GetTextExtentPoint32W,
    InvalidateRect, Rectangle, ReleaseDC, SelectObject, SetBkMode, SetTextColor, TextOutW,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, FONT_CHARSET, FONT_CLIP_PRECISION,
    FONT_OUTPUT_PRECISION, FONT_QUALITY, HDC, HGDIOBJ, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    GetWindowRect, KillTimer, LoadCursorW, LoadIconW, PostQuitMessage, RegisterClassExW, SetTimer,
    ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, IDC_ARROW,
    IDI_APPLICATION, MSG, SW_SHOW, WM_CHAR, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN,
    WM_LBUTTONUP, WM_PAINT, WM_TIMER, WNDCLASSEXW,
};

const WINDOW_WIDTH: i32 = 1020;
const WINDOW_HEIGHT: i32 = 700;
const LEFT_PANEL: i32 = 220;
const MSG_TOP: i32 = 56;
const MSG_BOTTOM: i32 = 610;
const INPUT_RECT: (i32, i32, i32, i32) = (240, 620, 800, 664);
const SEND_RECT: (i32, i32, i32, i32) = (815, 624, 985, 660);
const LINE_HEIGHT: i32 = 26;
const MAX_LINE_CHARS: usize = 25;

#[derive(Debug, Clone, Deserialize)]
struct IncomingEvent {
    #[serde(default)]
    from: Option<String>,
    #[serde(default = "default_delay")]
    delay_ms: u64,
    text: String,
}

fn default_delay() -> u64 {
    1000
}

#[derive(Debug, Clone, Deserialize)]
struct ReplySpec {
    #[serde(default = "default_reply_delay")]
    delay_ms: u64,
    text: String,
}

fn default_reply_delay() -> u64 {
    3000
}

#[derive(Debug, Clone, Deserialize)]
struct Scenario {
    contacts: Vec<String>,
    #[serde(default)]
    incoming: Vec<IncomingEvent>,
    #[serde(default)]
    replies: Vec<ReplySpec>,
}

#[derive(Debug, Clone)]
struct ChatMessage {
    from: String,
    text: String,
    is_self: bool,
}

struct ContactState {
    name: String,
    messages: Vec<ChatMessage>,
}

struct SimState {
    contacts: Vec<ContactState>,
    active: usize,
    input: String,
    scenario: Scenario,
    incoming_queue: VecDeque<(Instant, String, String)>,
    pending_reply: Option<(String, String, Instant)>,
    reply_index: usize,
    log_path: PathBuf,
    log_file: Option<std::fs::File>,
}

static STATE: OnceLock<Mutex<SimState>> = OnceLock::new();
static STRING_POOL: Mutex<Vec<Vec<u16>>> = Mutex::new(Vec::new());

fn main() {
    let mut scenario_path = None;
    let mut log_path = PathBuf::from("sim-qq-log.jsonl");
    let mut headless = false;
    let mut port = 18500;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scenario" => scenario_path = args.next(),
            "--headless" => headless = true,
            "--port" => {
                if let Some(value) = args.next() {
                    port = value.parse().unwrap_or(18500);
                }
            }
            "--log" => {
                if let Some(value) = args.next() {
                    log_path = PathBuf::from(value);
                }
            }
            _ => {}
        }
    }
    let scenario: Scenario = match scenario_path {
        Some(path) => {
            let content = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("读取场景 {path} 失败：{e}"));
            serde_json::from_str(&content).unwrap_or_else(|e| panic!("场景解析失败：{e}"))
        }
        None => serde_json::from_str(DEFAULT_SCENARIO).expect("默认场景解析失败"),
    };
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .unwrap_or_else(|e| panic!("打开日志 {log_path:?} 失败：{e}"));
    let contacts = scenario
        .contacts
        .iter()
        .map(|name| ContactState {
            name: name.clone(),
            messages: Vec::new(),
        })
        .collect();
    let state = SimState {
        contacts,
        active: 0,
        input: String::new(),
        scenario,
        incoming_queue: VecDeque::new(),
        pending_reply: None,
        reply_index: 0,
        log_path: log_path.clone(),
        log_file: Some(log_file),
    };
    let _ = STATE.set(Mutex::new(state));
    seed_incoming();
    if headless {
        run_headless(&log_path, port);
    } else {
        run_window(&log_path);
    }
}

fn seed_incoming() {
    if let Some(state) = STATE.get() {
        if let Ok(mut guard) = state.lock() {
            let now = Instant::now();
            let incoming = guard.scenario.incoming.clone();
            for event in &incoming {
                let from = event
                    .from
                    .clone()
                    .unwrap_or_else(|| guard.contacts[guard.active].name.clone());
                guard.incoming_queue.push_back((
                    now + Duration::from_millis(event.delay_ms),
                    from,
                    event.text.clone(),
                ));
            }
        }
    }
}

const DEFAULT_SCENARIO: &str = r#"{
  "contacts": ["张子豪", "李四", "26大创-智能输入法"],
  "incoming": [
    { "from": "张子豪", "delay_ms": 1500, "text": "在吗？帮我看看今晚吃什么好" },
    { "from": "张子豪", "delay_ms": 6000, "text": "想吃点清淡的，不想吃太辣" }
  ],
  "replies": [
    { "delay_ms": 3500, "text": "好呀，那你去楼下那家粤菜馆？他们家的粥不错" },
    { "delay_ms": 4500, "text": "行，那就这么定了，六点半见" }
  ]
}"#;

fn run_window(log_path: &PathBuf) {
    unsafe {
        let hinstance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
            .expect("获取模块句柄失败");
        let class_name = widestr("OwO_Sim_QQ_Window");
        let wnd_class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance.into(),
            hIcon: LoadIconW(None, IDI_APPLICATION).unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: CreateSolidBrush(rgb(255, 255, 255)),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: class_name,
            hIconSm: windows::Win32::UI::WindowsAndMessaging::HICON::default(),
        };
        let atom = RegisterClassExW(&wnd_class);
        if atom == 0 {
            eprintln!("窗口类注册失败：{}", std::io::Error::last_os_error());
            std::process::exit(1);
        }
        let title = widestr("OwO 模拟QQ - 张子豪");
        let hwnd = CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE::default(),
            class_name,
            title,
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE::default(),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            WINDOW_WIDTH,
            WINDOW_HEIGHT,
            None,
            None,
            Some(hinstance.into()),
            None,
        )
        .expect("创建窗口失败");
        let _ = ShowWindow(hwnd, SW_SHOW);

        let mut rect = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rect);
        log_ready(log_path, &rect);

        SetTimer(Some(hwnd), 1, 100, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn widestr(text: &str) -> PCWSTR {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    let pointer = wide.as_ptr();
    if let Ok(mut pool) = STRING_POOL.lock() {
        pool.push(wide);
    }
    PCWSTR(pointer)
}

fn log_ready(log_path: &PathBuf, rect: &RECT) {
    let event = serde_json::json!({
        "ts": now_iso(),
        "type": "ready",
        "window_rect": {
            "left": rect.left,
            "top": rect.top,
            "right": rect.right,
            "bottom": rect.bottom,
            "width": rect.right - rect.left,
            "height": rect.bottom - rect.top,
        }
    });
    append_log(log_path, &event);
}

fn now_iso() -> String {
    Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string()
}

fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    COLORREF((red as u32) | ((green as u32) << 8) | ((blue as u32) << 16))
}

fn append_log(log_path: &PathBuf, event: &serde_json::Value) {
    if let Some(state) = STATE.get() {
        if let Ok(mut guard) = state.lock() {
            if let Some(file) = guard.log_file.as_mut() {
                use std::io::Write;
                let _ = writeln!(file, "{}", event);
                let _ = file.flush();
                return;
            }
        }
    }
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
    {
        let _ = writeln!(file, "{}", event);
    }
}

fn log_event(kind: &str, payload: serde_json::Value) {
    if let Some(state) = STATE.get() {
        if let Ok(guard) = state.lock() {
            let path = guard.log_path.clone();
            let mut event = serde_json::json!({ "ts": now_iso(), "type": kind });
            if let serde_json::Value::Object(map) = &mut event {
                if let serde_json::Value::Object(extra) = payload {
                    map.extend(extra);
                }
            }
            drop(guard);
            append_log(&path, &event);
        }
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut paint);
            paint_window(hwnd, hdc);
            let _ = EndPaint(hwnd, &paint);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_CHAR => {
            let code = wparam.0 as u32;
            let mut events = Vec::new();
            if let Some(state) = STATE.get() {
                if let Ok(mut guard) = state.lock() {
                    match code {
                        0x08 => {
                            guard.input.pop();
                        }
                        0x0D | 0x0A => {
                            events = submit(&mut guard);
                        }
                        0x1B => {
                            guard.input.clear();
                        }
                        _ => {
                            if let Some(character) = char::from_u32(code) {
                                if !character.is_control() {
                                    guard.input.push(character);
                                }
                            }
                        }
                    }
                }
            }
            for (kind, payload) in events {
                log_event(&kind, payload);
            }
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let key = wparam.0 as u32;
            if key == 0x0D {
                let mut events = Vec::new();
                if let Some(state) = STATE.get() {
                    if let Ok(mut guard) = state.lock() {
                        events = submit(&mut guard);
                    }
                }
                for (kind, payload) in events {
                    log_event(&kind, payload);
                }
                invalidate(hwnd);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = (lparam.0 & 0xFFFF) as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i32;
            let events = handle_click(x, y);
            for (kind, payload) in events {
                log_event(&kind, payload);
            }
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == 1 {
                tick(hwnd);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = KillTimer(Some(hwnd), 1);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn in_rect(x: i32, y: i32, rect: (i32, i32, i32, i32)) -> bool {
    x >= rect.0 && x <= rect.2 && y >= rect.1 && y <= rect.3
}

fn invalidate(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
}

fn tick(hwnd: HWND) {
    let events = tick_state();
    if !events.is_empty() {
        for (kind, payload) in events {
            log_event(&kind, payload);
        }
        invalidate(hwnd);
    }
}

/// 推进模拟时间：注入到期的 incoming / 对方回复，返回待写日志的事件。
fn tick_state() -> Vec<(String, serde_json::Value)> {
    let mut events = Vec::new();
    if let Some(state) = STATE.get() {
        if let Ok(mut guard) = state.lock() {
            let now = Instant::now();
            while let Some((due, _from, _text)) = guard.incoming_queue.front() {
                if *due > now {
                    break;
                }
                let (due, from, text) = guard.incoming_queue.pop_front().unwrap();
                let target = guard
                    .contacts
                    .iter()
                    .position(|contact| contact.name == from)
                    .unwrap_or(guard.active);
                guard.contacts[target].messages.push(ChatMessage {
                    from: from.clone(),
                    text: text.clone(),
                    is_self: false,
                });
                events.push((
                    "incoming".to_string(),
                    serde_json::json!({ "from": from, "text": text, "due_after_ms": due.elapsed().as_millis() }),
                ));
            }
            if let Some((contact_name, text, due)) = guard.pending_reply.clone() {
                if now >= due {
                    guard.pending_reply = None;
                    let target = guard
                        .contacts
                        .iter()
                        .position(|contact| contact.name == contact_name)
                        .unwrap_or(guard.active);
                    guard.contacts[target].messages.push(ChatMessage {
                        from: contact_name.clone(),
                        text: text.clone(),
                        is_self: false,
                    });
                    events.push((
                        "incoming".to_string(),
                        serde_json::json!({ "from": contact_name, "text": text, "reply_to_outgoing": true }),
                    ));
                }
            }
        }
    }
    events
}

fn submit(state: &mut SimState) -> Vec<(String, serde_json::Value)> {
    let mut events = Vec::new();
    let text = state.input.trim().to_string();
    if text.is_empty() {
        return events;
    }
    let contact_name = state.contacts[state.active].name.clone();
    state.contacts[state.active].messages.push(ChatMessage {
        from: "我".to_string(),
        text: text.clone(),
        is_self: true,
    });
    events.push((
        "outgoing".to_string(),
        serde_json::json!({ "to": contact_name, "text": text }),
    ));
    state.input.clear();
    if !state.scenario.replies.is_empty() {
        let spec = &state.scenario.replies[state.reply_index % state.scenario.replies.len()];
        state.reply_index += 1;
        state.pending_reply = Some((
            contact_name,
            spec.text.clone(),
            Instant::now() + Duration::from_millis(spec.delay_ms),
        ));
    }
    events
}

fn paint_window(hwnd: HWND, hdc: HDC) {
    let mut rect = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rect);
        let width = rect.right - rect.left;
        paint_ui(hdc, width, rect.bottom - rect.top);
    }
}

/// 虚拟点击（窗口模式与 headless 模式共用）：返回待写日志的事件。
fn handle_click(x: i32, y: i32) -> Vec<(String, serde_json::Value)> {
    let mut events = Vec::new();
    if let Some(state) = STATE.get() {
        if let Ok(mut guard) = state.lock() {
            if in_rect(x, y, INPUT_RECT) {
                events.push((
                    "input_clicked".to_string(),
                    serde_json::json!({ "x": x, "y": y }),
                ));
            } else if in_rect(x, y, SEND_RECT) {
                events.push((
                    "send_clicked".to_string(),
                    serde_json::json!({ "x": x, "y": y }),
                ));
                events.extend(submit(&mut guard));
            } else if x < LEFT_PANEL && y > 48 {
                let index = ((y - 52) / 46) as usize;
                if index < guard.contacts.len() {
                    guard.active = index;
                    events.push((
                        "contact_switched".to_string(),
                        serde_json::json!({ "contact": guard.contacts[index].name }),
                    ));
                }
            }
        }
    }
    events
}

// ---------- headless：离屏渲染 + 本地 HTTP 虚拟窗口 ----------

fn run_headless(log_path: &std::path::Path, port: u16) {
    log_event(
        "ready",
        serde_json::json!({
            "surface": "headless",
            "rect": [0, 0, WINDOW_WIDTH, WINDOW_HEIGHT],
            "log": log_path.to_string_lossy(),
        }),
    );
    std::thread::spawn(|| loop {
        std::thread::sleep(Duration::from_millis(100));
        let events = tick_state();
        for (kind, payload) in events {
            log_event(&kind, payload);
        }
    });
    let listener = TcpListener::bind(("127.0.0.1", port))
        .unwrap_or_else(|e| panic!("绑定模拟 QQ 端口 {port} 失败：{e}"));
    eprintln!("owo-sim-qq headless listening on http://127.0.0.1:{port}");
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let _ = handle_sim_connection(stream);
            }
            Err(_) => continue,
        }
    }
}

fn handle_sim_connection(mut stream: TcpStream) -> Result<(), String> {
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut header_end: Option<usize> = None;
    let mut content_length = 0usize;
    loop {
        let read = stream.read(&mut chunk).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if header_end.is_none() {
            if let Some(position) = find_subslice(&buffer, b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&buffer[..position]);
                for line in headers.lines().skip(1) {
                    if let Some((key, value)) = line.split_once(':') {
                        if key.eq_ignore_ascii_case("content-length") {
                            content_length = value.trim().parse().unwrap_or(0);
                        }
                    }
                }
                header_end = Some(position + 4);
            }
        }
        if let Some(end) = header_end {
            if buffer.len() >= end + content_length {
                break;
            }
        }
    }
    if buffer.is_empty() {
        return Ok(());
    }
    let request = String::from_utf8_lossy(&buffer).to_string();
    let first_line = request.lines().next().unwrap_or("GET / HTTP/1.1");
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let (path, _query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), Some(query.to_string())),
        None => (target.clone(), None),
    };
    let body_start = request
        .find("\r\n\r\n")
        .map(|i| i + 4)
        .unwrap_or(request.len());
    let body = request[body_start.min(request.len())..].to_string();

    let response: Vec<u8> = match (method.as_str(), path.as_str()) {
        ("GET", "/frame") => match render_frame_bmp() {
            Some(bmp) => bmp_response(&bmp),
            None => json_response(500, r#"{"ok":false,"error":"渲染失败"}"#),
        },
        ("POST", "/click") => {
            let value: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            let x = value
                .get("x")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0) as i32;
            let y = value
                .get("y")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0) as i32;
            let events = handle_click(x, y);
            let kinds: Vec<String> = events.iter().map(|(kind, _)| kind.clone()).collect();
            for (kind, payload) in events {
                log_event(&kind, payload);
            }
            json_response(
                200,
                &serde_json::json!({ "ok": true, "x": x, "y": y, "events": kinds }).to_string(),
            )
        }
        ("POST", "/type") => {
            let value: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            let text = value
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            handle_type(text);
            json_response(
                200,
                &serde_json::json!({ "ok": true, "typed_chars": text.chars().count() }).to_string(),
            )
        }
        ("POST", "/key") => {
            let value: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            let key = value
                .get("key")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let events = handle_key(key);
            for (kind, payload) in events {
                log_event(&kind, payload);
            }
            json_response(
                200,
                &serde_json::json!({ "ok": true, "key": key }).to_string(),
            )
        }
        ("POST", "/reset") => {
            reset_sim();
            json_response(200, r#"{"ok":true,"reset":true}"#)
        }
        ("GET", "/state") => json_response(200, &sim_state_json().to_string()),
        ("GET", "/ocr") => json_response(200, &sim_ocr_json().to_string()),
        ("GET", "/log") => json_response(200, &sim_log_json().to_string()),
        _ => json_response(404, r#"{"ok":false,"error":"not found"}"#),
    };
    let _ = stream.write_all(&response);
    let _ = stream.flush();
    Ok(())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn handle_type(text: &str) {
    if let Some(state) = STATE.get() {
        if let Ok(mut guard) = state.lock() {
            for character in text.chars() {
                if !character.is_control() {
                    guard.input.push(character);
                }
            }
        }
    }
}

fn reset_sim() {
    if let Some(state) = STATE.get() {
        if let Ok(mut guard) = state.lock() {
            for contact in &mut guard.contacts {
                contact.messages.clear();
            }
            guard.input.clear();
            guard.pending_reply = None;
            guard.reply_index = 0;
            guard.incoming_queue.clear();
            // append 句柄截断不可靠，直接重建日志文件。
            guard.log_file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&guard.log_path)
                .ok();
            let now = Instant::now();
            let incoming = guard.scenario.incoming.clone();
            for event in &incoming {
                let from = event
                    .from
                    .clone()
                    .unwrap_or_else(|| guard.contacts[guard.active].name.clone());
                guard.incoming_queue.push_back((
                    now + Duration::from_millis(event.delay_ms),
                    from,
                    event.text.clone(),
                ));
            }
        }
    }
}

fn handle_key(key: &str) -> Vec<(String, serde_json::Value)> {
    let mut events = Vec::new();
    if let Some(state) = STATE.get() {
        if let Ok(mut guard) = state.lock() {
            match key.to_lowercase().as_str() {
                "enter" | "return" => events = submit(&mut guard),
                "backspace" => {
                    guard.input.pop();
                }
                "escape" => guard.input.clear(),
                _ => {
                    if let Some(character) = key.chars().next() {
                        if !character.is_control() {
                            guard.input.push(character);
                        }
                    }
                }
            }
        }
    }
    events
}

fn sim_state_json() -> serde_json::Value {
    if let Some(state) = STATE.get() {
        if let Ok(guard) = state.lock() {
            let contacts: Vec<serde_json::Value> = guard
                .contacts
                .iter()
                .map(|contact| {
                    let messages: Vec<serde_json::Value> = contact
                        .messages
                        .iter()
                        .map(|message| {
                            serde_json::json!({
                                "from": message.from,
                                "text": message.text,
                                "is_self": message.is_self,
                            })
                        })
                        .collect();
                    serde_json::json!({ "name": contact.name, "messages": messages })
                })
                .collect();
            return serde_json::json!({
                "surface": "headless",
                "active_contact": guard.contacts[guard.active].name,
                "input": guard.input,
                "pending_reply": guard.pending_reply.is_some(),
                "contacts": contacts,
            });
        }
    }
    serde_json::json!({ "error": "state unavailable" })
}

/// 模拟窗口的“真值版面”：Agent 在模拟面上读取的 OCR 结果。
/// 由状态直接生成（文字+坐标+role_hint），不依赖 Media.Ocr 对离屏渲染的识别质量。
fn sim_ocr_json() -> serde_json::Value {
    if let Some(state) = STATE.get() {
        if let Ok(guard) = state.lock() {
            let mut lines: Vec<serde_json::Value> = Vec::new();
            let width = WINDOW_WIDTH;
            for (index, contact) in guard.contacts.iter().enumerate() {
                let y = 52 + index as i32 * 46;
                lines.push(sim_ocr_line(
                    contact.name.clone(),
                    14,
                    y + 8,
                    LEFT_PANEL - 24,
                    22,
                    if index == guard.active {
                        "active_contact"
                    } else {
                        "contact"
                    },
                ));
                let last = contact
                    .messages
                    .last()
                    .map(|message| message.text.as_str())
                    .unwrap_or("");
                lines.push(sim_ocr_line(
                    truncate(last, 12),
                    14,
                    y + 30,
                    LEFT_PANEL - 24,
                    18,
                    "preview",
                ));
            }
            lines.push(sim_ocr_line(
                format!("与 {} 聊天中", guard.contacts[guard.active].name),
                244,
                12,
                500,
                24,
                "header",
            ));

            let messages = &guard.contacts[guard.active].messages;
            let start = messages.len().saturating_sub(18);
            let mut y = MSG_TOP;
            for message in &messages[start..] {
                lines.push(sim_ocr_line(
                    message.from.clone(),
                    240,
                    y,
                    400,
                    22,
                    if message.is_self {
                        "self_name"
                    } else {
                        "contact_name"
                    },
                ));
                y += 24;
                for text_line in wrap_text(&message.text, MAX_LINE_CHARS) {
                    let x = if message.is_self { width - 500 } else { 250 };
                    lines.push(sim_ocr_line(text_line, x, y, 500, LINE_HEIGHT, "message"));
                    y += LINE_HEIGHT;
                }
                y += 6;
                if y > MSG_BOTTOM - 30 {
                    break;
                }
            }

            let input_text = if guard.input.is_empty() {
                "输入消息...".to_string()
            } else {
                truncate(&guard.input, 34)
            };
            lines.push(sim_ocr_line(
                input_text,
                INPUT_RECT.0 + 12,
                INPUT_RECT.1 + 10,
                500,
                26,
                "input",
            ));
            lines.push(sim_ocr_line(
                "发送".to_string(),
                SEND_RECT.0,
                SEND_RECT.1,
                SEND_RECT.2 - SEND_RECT.0,
                SEND_RECT.3 - SEND_RECT.1,
                "button",
            ));
            let status = if guard.pending_reply.is_some() {
                "对方正在输入…".to_string()
            } else {
                "模拟QQ · 已连接".to_string()
            };
            lines.push(sim_ocr_line(status, 240, MSG_BOTTOM + 8, 400, 18, "status"));

            let text: String = lines
                .iter()
                .filter_map(|line| line.get("text").and_then(serde_json::Value::as_str))
                .collect::<Vec<_>>()
                .join(" ");
            return serde_json::json!({
                "surface": "headless",
                "text": text,
                "chars": text.chars().count(),
                "lines": lines,
                "box_count": 0,
            });
        }
    }
    serde_json::json!({ "error": "state unavailable" })
}

fn sim_ocr_line(
    text: String,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    role_hint: &str,
) -> serde_json::Value {
    serde_json::json!({
        "text": text,
        "x": x,
        "y": y,
        "width": width,
        "height": height,
        "role_hint": role_hint,
    })
}

fn sim_log_json() -> serde_json::Value {
    let mut entries = Vec::new();
    if let Some(state) = STATE.get() {
        if let Ok(guard) = state.lock() {
            if let Ok(content) = std::fs::read_to_string(&guard.log_path) {
                for line in content.lines() {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                        entries.push(value);
                    }
                }
            }
        }
    }
    serde_json::json!({ "entries": entries })
}

fn render_frame_bmp() -> Option<Vec<u8>> {
    unsafe {
        let screen = GetDC(None);
        let memory = CreateCompatibleDC(Some(screen));
        let bitmap = CreateCompatibleBitmap(screen, WINDOW_WIDTH, WINDOW_HEIGHT);
        if memory.0.is_null() || bitmap.0.is_null() {
            let _ = ReleaseDC(None, screen);
            return None;
        }
        let old = SelectObject(memory, HGDIOBJ::from(bitmap));
        paint_ui(memory, WINDOW_WIDTH, WINDOW_HEIGHT);
        SelectObject(memory, old);

        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = WINDOW_WIDTH;
        info.bmiHeader.biHeight = -WINDOW_HEIGHT;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB.0;
        let mut pixels = vec![0u8; (WINDOW_WIDTH as usize) * (WINDOW_HEIGHT as usize) * 4];
        let got = GetDIBits(
            memory,
            bitmap,
            0,
            WINDOW_HEIGHT as u32,
            Some(pixels.as_mut_ptr() as *mut core::ffi::c_void),
            &mut info,
            DIB_RGB_COLORS,
        );
        let _ = DeleteObject(HGDIOBJ::from(bitmap));
        let _ = DeleteDC(memory);
        let _ = ReleaseDC(None, screen);
        if got == 0 {
            return None;
        }
        Some(encode_bmp(WINDOW_WIDTH, WINDOW_HEIGHT, &pixels))
    }
}

fn encode_bmp(width: i32, height: i32, bgra: &[u8]) -> Vec<u8> {
    let file_size = 54 + bgra.len();
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
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(bgra.len() as u32).to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(bgra);
    out
}

fn json_response(status: u16, body: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(
        format!("HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", status_text(status), body.len()).as_bytes(),
    );
    out.extend_from_slice(body.as_bytes());
    out
}

fn bmp_response(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: image/bmp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            bytes.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(bytes);
    out
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Unknown",
    }
}

fn paint_ui(hdc: HDC, width: i32, height: i32) {
    unsafe {
        let rect = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        let white = CreateSolidBrush(rgb(255, 255, 255));
        let panel = CreateSolidBrush(rgb(244, 244, 246));
        let active_row = CreateSolidBrush(rgb(222, 236, 255));
        let header = CreateSolidBrush(rgb(238, 238, 240));
        let blue = CreateSolidBrush(rgb(45, 122, 255));
        let border = CreateSolidBrush(rgb(205, 205, 210));

        FillRect(hdc, &rect, white);
        let mut panel_rect = rect;
        panel_rect.right = LEFT_PANEL;
        FillRect(hdc, &panel_rect, panel);
        let mut header_rect = rect;
        header_rect.top = 0;
        header_rect.bottom = 48;
        header_rect.left = LEFT_PANEL;
        FillRect(hdc, &header_rect, header);

        let normal_font = CreateFontW(
            -22,
            0,
            0,
            0,
            500,
            0,
            0,
            0,
            FONT_CHARSET(134),
            FONT_OUTPUT_PRECISION(0),
            FONT_CLIP_PRECISION(0),
            FONT_QUALITY(0),
            0,
            PCWSTR(widestr("Microsoft YaHei").0),
        );
        let bold_font = CreateFontW(
            -26,
            0,
            0,
            0,
            700,
            0,
            0,
            0,
            FONT_CHARSET(134),
            FONT_OUTPUT_PRECISION(0),
            FONT_CLIP_PRECISION(0),
            FONT_QUALITY(0),
            0,
            PCWSTR(widestr("Microsoft YaHei").0),
        );
        let small_font = CreateFontW(
            -17,
            0,
            0,
            0,
            500,
            0,
            0,
            0,
            FONT_CHARSET(134),
            FONT_OUTPUT_PRECISION(0),
            FONT_CLIP_PRECISION(0),
            FONT_QUALITY(0),
            0,
            PCWSTR(widestr("Microsoft YaHei").0),
        );

        let old = SelectObject(hdc, HGDIOBJ::from(normal_font));
        SetBkMode(hdc, TRANSPARENT);

        // 左侧会话列表
        let old_bold = SelectObject(hdc, HGDIOBJ::from(bold_font));
        SetTextColor(hdc, rgb(60, 60, 60));
        text_out(hdc, 14, 14, "会话");
        SelectObject(hdc, old_bold);

        if let Some(state) = STATE.get() {
            if let Ok(guard) = state.lock() {
                for (index, contact) in guard.contacts.iter().enumerate() {
                    let y = 52 + index as i32 * 46;
                    if index == guard.active {
                        let row = RECT {
                            left: 4,
                            top: y,
                            right: LEFT_PANEL - 4,
                            bottom: y + 42,
                        };
                        FillRect(hdc, &row, active_row);
                    }
                    SetTextColor(hdc, rgb(30, 30, 30));
                    text_out(hdc, 14, y + 11, &contact.name);
                    SetTextColor(hdc, rgb(130, 130, 130));
                    let last = contact
                        .messages
                        .last()
                        .map(|message| message.text.as_str())
                        .unwrap_or("");
                    text_out(hdc, 14, y + 33, &truncate(last, 12));
                }

                // 右侧头部
                let active_name = &guard.contacts[guard.active].name;
                let old_bold = SelectObject(hdc, HGDIOBJ::from(bold_font));
                SetTextColor(hdc, rgb(40, 40, 40));
                text_out(hdc, 244, 12, &format!("与 {active_name} 聊天中"));
                SelectObject(hdc, old_bold);

                // 消息区
                let messages = &guard.contacts[guard.active].messages;
                let start = messages.len().saturating_sub(18);
                let mut y = MSG_TOP;
                SetTextColor(hdc, rgb(0, 0, 0));
                for message in &messages[start..] {
                    let name_color = if message.is_self {
                        rgb(0, 120, 90)
                    } else {
                        rgb(20, 80, 180)
                    };
                    SetTextColor(hdc, name_color);
                    text_out(hdc, 240, y, &message.from);
                    y += 24;
                    SetTextColor(hdc, rgb(0, 0, 0));
                    for line in wrap_text(&message.text, MAX_LINE_CHARS) {
                        let x = if message.is_self { width - 500 } else { 250 };
                        text_out(hdc, x, y, &line);
                        y += LINE_HEIGHT;
                    }
                    y += 6;
                    if y > MSG_BOTTOM - 30 {
                        break;
                    }
                }

                // 输入框与发送按钮
                let input_rect = RECT {
                    left: INPUT_RECT.0,
                    top: INPUT_RECT.1,
                    right: INPUT_RECT.2,
                    bottom: INPUT_RECT.3,
                };
                FillRect(hdc, &input_rect, white);
                let old_pen = SelectObject(hdc, HGDIOBJ::from(border));
                let _ = Rectangle(
                    hdc,
                    input_rect.left,
                    input_rect.top,
                    input_rect.right,
                    input_rect.bottom,
                );
                SelectObject(hdc, old_pen);
                if guard.input.is_empty() {
                    SetTextColor(hdc, rgb(70, 70, 70));
                    text_out(hdc, INPUT_RECT.0 + 12, INPUT_RECT.1 + 10, "输入消息...");
                } else {
                    SetTextColor(hdc, rgb(0, 0, 0));
                    let visible = truncate(&guard.input, 34);
                    text_out(hdc, INPUT_RECT.0 + 12, INPUT_RECT.1 + 10, &visible);
                }

                let send_rect = RECT {
                    left: SEND_RECT.0,
                    top: SEND_RECT.1,
                    right: SEND_RECT.2,
                    bottom: SEND_RECT.3,
                };
                let light_blue = CreateSolidBrush(rgb(226, 237, 255));
                FillRect(hdc, &send_rect, light_blue);
                let old_bold = SelectObject(hdc, HGDIOBJ::from(bold_font));
                SetTextColor(hdc, rgb(10, 60, 140));
                let send_text = "发送";
                let (text_w, _) = text_size(hdc, send_text);
                text_out(
                    hdc,
                    SEND_RECT.0 + (SEND_RECT.2 - SEND_RECT.0 - text_w) / 2,
                    SEND_RECT.1 + 7,
                    send_text,
                );
                SelectObject(hdc, old_bold);
                let _ = DeleteObject(HGDIOBJ::from(light_blue));

                // 底部状态
                let old_small = SelectObject(hdc, HGDIOBJ::from(small_font));
                SetTextColor(hdc, rgb(120, 120, 120));
                let status = if guard.pending_reply.is_some() {
                    "对方正在输入…"
                } else {
                    "模拟QQ · 已连接"
                };
                text_out(hdc, 240, MSG_BOTTOM + 8, status);
                SelectObject(hdc, old_small);
            }
        }

        SelectObject(hdc, old);
        let _ = DeleteObject(HGDIOBJ::from(normal_font));
        let _ = DeleteObject(HGDIOBJ::from(bold_font));
        let _ = DeleteObject(HGDIOBJ::from(small_font));
        let _ = DeleteObject(HGDIOBJ::from(white));
        let _ = DeleteObject(HGDIOBJ::from(panel));
        let _ = DeleteObject(HGDIOBJ::from(active_row));
        let _ = DeleteObject(HGDIOBJ::from(header));
        let _ = DeleteObject(HGDIOBJ::from(blue));
        let _ = DeleteObject(HGDIOBJ::from(border));
    }
}

fn text_out(hdc: HDC, x: i32, y: i32, text: &str) {
    unsafe {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let _ = TextOutW(hdc, x, y, &wide);
    }
}

fn text_size(hdc: HDC, text: &str) -> (i32, i32) {
    unsafe {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let mut size = SIZE::default();
        let _ = GetTextExtentPoint32W(hdc, &wide, &mut size);
        (size.cx, size.cy)
    }
}

fn wrap_text(text: &str, max_chars: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut lines = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let end = (index + max_chars).min(chars.len());
        lines.push(chars[index..end].iter().collect());
        index = end;
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn truncate(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        text.to_string()
    } else {
        let mut result: String = chars[..max_chars.saturating_sub(1)].iter().collect();
        result.push('…');
        result
    }
}
