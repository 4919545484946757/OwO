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
use tauri_plugin_updater::UpdaterExt;

const CORE_PORT: u16 = 4096;

struct CoreServer(Mutex<Option<Child>>);
struct AutostartState(Mutex<bool>);

/// 开发环境定位核心服务：`<repo>/agent-sdk/target/debug/owo-agent.exe`。
fn core_server_path() -> PathBuf {
    // 便携发布：核心服务与桌面壳同级。
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let bundled = dir.join("owo-agent-x64.exe");
            if bundled.exists() {
                return bundled;
            }
            let sibling = dir.join("owo-agent.exe");
            if sibling.exists() {
                return sibling;
            }
        }
    }
    // 开发环境：<repo>/agent-sdk/target/debug/owo-agent.exe。
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
    let portable = exe
        .file_name()
        .map(|name| name.to_string_lossy().contains("-x64"))
        .unwrap_or(false)
        || exe
            .parent()
            .map(|dir| dir.join("owo-agent-desktop.exe").exists())
            .unwrap_or(false);
    let workspace = if portable {
        exe.parent()?.to_path_buf() // 便携发布：应用目录
    } else {
        exe.parent()?.parent()?.parent()?.to_path_buf() // 开发：agent-sdk
    };
    let port = CORE_PORT.to_string();
    let mut command = Command::new(exe);
    command
        .args(["serve", "--port"])
        .arg(&port)
        .arg("--workspace")
        .arg(workspace);

    // 桌面壳必须能先把本地服务拉起。没有云端凭据时，核心服务会接受
    // OpenAI 兼容的本地端点；这避免了 UI 已打开、后端却因缺少 API key
    // 立即退出，从而把所有面板都变成 connection refused。
    if std::env::var_os("OPENAI_API_KEY").is_none() && std::env::var_os("OPENAI_BASE_URL").is_none()
    {
        if let Some(token_plan_key) = std::env::var_os("DASHSCOPE_API_KEY") {
            // 千问 Token Plan 专属密钥必须配套使用该端点；不把密钥写入
            // settings.json，子进程仅从用户环境变量继承它。
            command.env("OPENAI_API_KEY", token_plan_key).env(
                "OPENAI_BASE_URL",
                "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
            );
        } else {
            command
                .env("OPENAI_BASE_URL", "http://127.0.0.1:11434/v1")
                .env("OPENAI_MODEL", "local");
        }
    }

    command.spawn().ok()
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn run_key() -> std::io::Result<winreg::RegKey> {
    use winreg::enums::HKEY_CURRENT_USER;
    let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
    hkcu.create_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run")
        .map(|(key, _)| key)
}

fn autostart_enabled() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    let Ok(key) = winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run")
    else {
        return false;
    };
    let Ok(value) = key.get_value::<String, _>("OwOAgentDesktop") else {
        return false;
    };
    value.contains("owo-agent-desktop")
}

fn set_autostart(enabled: bool) -> std::io::Result<()> {
    let key = run_key()?;
    if enabled {
        let exe = std::env::current_exe()?;
        key.set_value("OwOAgentDesktop", &format!("\"{}\"", exe.display()))?;
    } else {
        key.delete_value("OwOAgentDesktop")?;
    }
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            let child = spawn_core_server();
            app.manage(CoreServer(Mutex::new(child)));
            app.manage(AutostartState(Mutex::new(autostart_enabled())));

            // 全局快捷键：Ctrl+Alt+Shift+O 唤起工作台（避免常见冲突）。
            let shortcut = Shortcut::new(
                Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT),
                Code::KeyO,
            );
            let _ = app
                .global_shortcut()
                .on_shortcut(shortcut, |app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        show_main_window(app);
                    }
                });
            if let Err(error) = app.global_shortcut().register(shortcut) {
                eprintln!("[owo-desktop] 全局快捷键注册失败（继续运行）：{error}");
            }

            // 托盘：显示 / 退出。
            let show = MenuItem::with_id(app, "show", "显示工作台", true, None::<&str>)?;
            let autostart_label = if autostart_enabled() {
                "开机自启：开"
            } else {
                "开机自启：关"
            };
            let autostart =
                MenuItem::with_id(app, "autostart", autostart_label, true, None::<&str>)?;
            let check_update =
                MenuItem::with_id(app, "check-update", "检查更新", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &autostart, &check_update, &quit])?;
            let icon = app.default_window_icon().cloned().ok_or("缺少应用图标")?;
            let _tray = TrayIconBuilder::new()
                .icon(icon)
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main_window(app),
                    "autostart" => {
                        let enabled = if let Some(state) = app.try_state::<AutostartState>() {
                            let mut guard = state.0.lock().unwrap();
                            let next = !*guard;
                            if set_autostart(next).is_ok() {
                                *guard = next;
                            }
                            *guard
                        } else {
                            false
                        };
                        if let Some(menu) = app.menu() {
                            if let Some(item) = menu.get("autostart") {
                                if let Some(menuitem) = item.as_menuitem() {
                                    let _ = menuitem.set_text(if enabled {
                                        "开机自启：开"
                                    } else {
                                        "开机自启：关"
                                    });
                                }
                            }
                        }
                    }
                    "check-update" => {
                        let handle = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let result = match handle.updater() {
                                Ok(updater) => updater.check().await,
                                Err(error) => Err(error),
                            };
                            match result {
                                Ok(Some(update)) => {
                                    eprintln!(
                                        "[owo-desktop] 发现新版本 {}：{}",
                                        update.version,
                                        update.body.unwrap_or_default()
                                    );
                                    if let Some(menu) = handle.menu() {
                                        if let Some(item) = menu.get("check-update") {
                                            if let Some(menuitem) = item.as_menuitem() {
                                                let _ = menuitem.set_text("检查更新（有新版本）");
                                            }
                                        }
                                    }
                                }
                                Ok(None) => {
                                    if let Some(menu) = handle.menu() {
                                        if let Some(item) = menu.get("check-update") {
                                            if let Some(menuitem) = item.as_menuitem() {
                                                let _ = menuitem.set_text("检查更新（已是最新）");
                                            }
                                        }
                                    }
                                }
                                Err(error) => {
                                    eprintln!("[owo-desktop] 检查更新失败：{error}");
                                }
                            }
                        });
                    }
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
