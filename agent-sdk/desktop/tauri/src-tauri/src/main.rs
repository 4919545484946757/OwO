#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! OwO Agent 桌面主客户端（v0.4 P1）：Tauri 2 窗口壳 + 常驻核心服务 + 全局快捷键 + 托盘。
//!
//! 壳本身无状态：加载 `desktop/web` 工作台，通过本机 HTTP 访问
//! `owo-agent serve` 常驻核心服务；退出时自动结束核心服务子进程。

use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Mutex;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, RunEvent};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

const CORE_PORT: u16 = 4096;

struct CoreServer(Mutex<Option<Child>>);

/// 开发环境定位核心服务：`<repo>/agent-sdk/target/debug/owo-agent.exe`。
fn core_server_path() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")); // desktop/tauri/src-tauri
    manifest
        .parent() // desktop/tauri
        .and_then(|parent| parent.parent()) // desktop
        .and_then(|parent| parent.parent()) // agent-sdk
        .map(|root| root.join("target").join("debug").join("owo-agent.exe"))
        .unwrap_or_else(|| PathBuf::from("owo-agent.exe"))
}

fn spawn_core_server() -> Option<Child> {
    let exe = core_server_path();
    if !exe.exists() {
        eprintln!("[owo-desktop] 核心服务不存在：{}", exe.display());
        return None;
    }
    let workspace = exe
        .parent()?
        .parent()?
        .parent()?
        .to_path_buf(); // agent-sdk
    let port = CORE_PORT.to_string();
    Command::new(exe)
        .args(["serve", "--port"])
        .arg(&port)
        .arg("--workspace")
        .arg(workspace)
        .spawn()
        .ok()
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            let child = spawn_core_server();
            app.manage(CoreServer(Mutex::new(child)));

            // 全局快捷键：Ctrl+Alt+Shift+O 唤起工作台（避免常见冲突）。
            let shortcut = Shortcut::new(
                Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT),
                Code::KeyO,
            );
            let _ = app.global_shortcut().on_shortcut(
                shortcut,
                |app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        show_main_window(app);
                    }
                },
            );
            if let Err(error) = app.global_shortcut().register(shortcut) {
                eprintln!("[owo-desktop] 全局快捷键注册失败（继续运行）：{error}");
            }

            // 托盘：显示 / 退出。
            let show = MenuItem::with_id(app, "show", "显示工作台", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            let icon = app
                .default_window_icon()
                .cloned()
                .ok_or("缺少应用图标")?;
            let _tray = TrayIconBuilder::new()
                .icon(icon)
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("构建 OwO Agent 桌面应用失败")
        .run(|app_handle, event| {
            if let RunEvent::Exit = event {
                if let Some(state) = app_handle.try_state::<CoreServer>() {
                    if let Ok(mut guard) = state.0.lock() {
                        if let Some(mut child) = guard.take() {
                            let _ = child.kill();
                        }
                    }
                }
            }
        });
}
