//! R8:<storage> 数据备份/恢复/导出/清空 完成，待主控接线。
//!
//! 存储运维四路由：
//! - POST /storage/backup ：zip 原样打包 index.db + sessions + skills + workflows + notes + settings（排除缓存/模型）；
//! - POST /storage/restore：先自动备份再恢复（zip 安全校验；index.db 校验后暂存、重启生效）；
//! - POST /storage/export ：全量标准 JSON 导出（sessions/audit/notes/skills/workflows/settings）；
//! - POST /storage/clear  ：二次确认（{"confirm":"CLEAR_ALL"}）后一键清空，清空后完整性校验。
//!
//! 模块约定：不引用 `crate::`/`super::`（AppState 全限定），可被测试以 #[path] mod 独立编译。

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use owo_agent_core::notes;
use owo_agent_core::sqlite_store::SqliteSessionStore;
use owo_agent_server::AppState;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zip::write::SimpleFileOptions;

/// 恢复包安全上限：条目数 / 单条目未压缩上限 / 总量上限。
const MAX_ARCHIVE_ENTRIES: usize = 5_000;
const MAX_ENTRY_SIZE: u64 = 512 * 1024 * 1024;
const MAX_TOTAL_SIZE: u64 = 2 * 1024 * 1024 * 1024;

/// 清空操作的二次确认令牌。
pub const CLEAR_CONFIRM: &str = "CLEAR_ALL";

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/storage/backup", post(backup_handler))
        .route("/storage/restore", post(restore_handler))
        .route("/storage/export", post(export_handler))
        .route("/storage/clear", post(clear_handler))
        .with_state(state)
}

fn now_stamp() -> String {
    let now = chrono::Utc::now();
    now.format("%Y%m%d-%H%M%S").to_string()
}

fn backups_dir(data_root: &Path) -> PathBuf {
    data_root.join("backups")
}

/// 备份源清单：(zip 内路径, 磁盘绝对路径)。排除缓存/模型（models/、traces/、eval/、backups/ 自身）。
fn collect_backup_sources(data_root: &Path, workspace: &Path) -> Vec<(String, PathBuf)> {
    let mut sources: Vec<(String, PathBuf)> = Vec::new();
    let push = |sources: &mut Vec<(String, PathBuf)>, zip_name: &str, path: &Path| {
        if path.is_file() {
            sources.push((zip_name.to_string(), path.to_path_buf()));
        }
    };
    push(&mut sources, "index.db", &data_root.join("index.db"));
    push(
        &mut sources,
        "index.db-wal",
        &data_root.join("index.db-wal"),
    );
    push(
        &mut sources,
        "index.db-shm",
        &data_root.join("index.db-shm"),
    );
    push(
        &mut sources,
        "settings.json",
        &workspace.join("settings.json"),
    );
    push(
        &mut sources,
        "memory.jsonl",
        &data_root.join("memory.jsonl"),
    );
    push(
        &mut sources,
        "plugin_state.json",
        &data_root.join("plugin_state.json"),
    );
    push(
        &mut sources,
        "automations.json",
        &data_root.join("automations.json"),
    );
    collect_dir_recursive(&mut sources, &data_root.join("notes"), "notes");
    collect_dir_recursive(&mut sources, &data_root.join("skills"), "skills");
    collect_dir_recursive(&mut sources, &data_root.join("goals"), "goals");
    collect_dir_recursive(
        &mut sources,
        &data_root.join("intent-workflows"),
        "intent-workflows",
    );
    collect_dir_recursive(
        &mut sources,
        &data_root.join("eval").join("reports"),
        "eval/reports",
    );
    collect_owflow_recursive(&mut sources, workspace);
    sources.sort_by(|left, right| left.0.cmp(&right.0));
    sources
}

fn collect_dir_recursive(sources: &mut Vec<(String, PathBuf)>, dir: &Path, prefix: &str) {
    if !dir.is_dir() {
        return;
    }
    let mut stack = vec![(dir.to_path_buf(), prefix.to_string())];
    while let Some((current, zip_prefix)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let zip_name = format!("{zip_prefix}/{name}");
            if path.is_dir() {
                stack.push((path, zip_name));
            } else if path.is_file() {
                sources.push((zip_name, path));
            }
        }
    }
}

fn collect_owflow_recursive(sources: &mut Vec<(String, PathBuf)>, workspace: &Path) {
    let mut stack = vec![workspace.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("owflow") {
                let relative = path
                    .strip_prefix(workspace)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                sources.push((format!("workflows/{relative}"), path));
            }
        }
    }
}

/// 构建备份 zip（内存），附 manifest.json。
pub fn build_backup_zip(data_root: &Path, workspace: &Path) -> Result<Vec<u8>, String> {
    let sources = collect_backup_sources(data_root, workspace);
    let mut buffer: Vec<u8> = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        let manifest = json!({
            "app": "owo-agent",
            "kind": "backup",
            "created_at": chrono::Utc::now().to_rfc3339(),
            "entries": sources.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            "excluded": ["models/", "traces/", "eval/reports/cache", "backups/"],
        });
        writer
            .start_file("manifest.json", options)
            .map_err(|e| format!("zip manifest 失败：{e}"))?;
        writer
            .write_all(
                serde_json::to_vec_pretty(&manifest)
                    .map_err(|e| format!("manifest 序列化失败：{e}"))?
                    .as_slice(),
            )
            .map_err(|e| format!("zip manifest 写入失败：{e}"))?;
        for (zip_name, path) in &sources {
            let content = std::fs::read(path)
                .map_err(|e| format!("备份读取 {} 失败：{e}", path.display()))?;
            writer
                .start_file(zip_name, options)
                .map_err(|e| format!("zip 条目 {zip_name} 失败：{e}"))?;
            writer
                .write_all(&content)
                .map_err(|e| format!("zip 写入 {zip_name} 失败：{e}"))?;
        }
        writer.finish().map_err(|e| format!("zip 收尾失败：{e}"))?;
    }
    Ok(buffer)
}

/// zip 条目名安全校验：拒绝绝对路径、`..`、空段（防 zip-slip）。
fn safe_entry_name(name: &str) -> Result<String, String> {
    let normalized = name.trim_start_matches('/').replace('\\', "/");
    let mut clean = String::new();
    for component in normalized.split('/') {
        match component {
            "" | "." => {}
            ".." => return Err(format!("非法条目路径：{name}")),
            other => {
                if !clean.is_empty() {
                    clean.push('/');
                }
                clean.push_str(other);
            }
        }
    }
    if clean.is_empty() {
        return Err(format!("空条目路径：{name}"));
    }
    Ok(clean)
}

/// 恢复 zip 解析结果：清单 + 已安全化条目（相对路径, 内容）。
type RestoreEntries = Vec<(String, Vec<u8>)>;
type RestoreParse = Result<(Value, RestoreEntries), String>;

/// 解析恢复 zip：返回 (manifest, [(safe 相对路径, 内容)])。
fn parse_restore_zip(bytes: &[u8]) -> RestoreParse {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| format!("不是有效的 zip 备份包：{e}"))?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(format!("备份包条目过多：{}", archive.len()));
    }
    let mut manifest = json!({});
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut total: u64 = 0;
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|e| format!("读取备份条目失败：{e}"))?;
        let raw_name = file.name().to_string();
        let safe = safe_entry_name(&raw_name)?;
        let size = file.size();
        if size > MAX_ENTRY_SIZE {
            return Err(format!("条目 {safe} 超限（{size} 字节）"));
        }
        total = total.saturating_add(size);
        if total > MAX_TOTAL_SIZE {
            return Err("备份包总量超限".to_string());
        }
        let mut content = Vec::with_capacity(size as usize);
        file.read_to_end(&mut content)
            .map_err(|e| format!("读取条目 {safe} 失败：{e}"))?;
        if safe == "manifest.json" {
            manifest = serde_json::from_slice(&content)
                .map_err(|e| format!("manifest.json 解析失败：{e}"))?;
        } else {
            entries.push((safe, content));
        }
    }
    Ok((manifest, entries))
}

fn write_staged(data_root: &Path, relative: &str, content: &[u8]) -> Result<(), String> {
    // index.db 由调用方单独处理；其余写回磁盘。
    let target = data_root.join(relative);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = target.with_extension("restoring");
    std::fs::write(&tmp, content).map_err(|e| format!("写入 {} 失败：{e}", target.display()))?;
    std::fs::rename(&tmp, &target).map_err(|e| format!("替换 {} 失败：{e}", target.display()))?;
    Ok(())
}

/// 校验恢复的 index.db（经核心存储打开 + PRAGMA integrity_check），返回校验结果。
fn validate_sqlite(bytes: &[u8]) -> Result<String, String> {
    let path = std::env::temp_dir().join(format!("owo-restore-check-{}.db", uuid::Uuid::new_v4()));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    let result = (|| -> Result<String, String> {
        let store = SqliteSessionStore::open(&path).map_err(|e| e.to_string())?;
        store.integrity_check().map_err(|e| e.to_string())
    })();
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    result
}

async fn backup_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (axum::http::StatusCode, String)> {
    let backup_dir = backups_dir(&state.data_root);
    std::fs::create_dir_all(&backup_dir).map_err(internal)?;
    let zip_bytes = build_backup_zip(&state.data_root, &state.workspace).map_err(internal)?;
    let saved_to = backup_dir.join(format!("backup-{}.zip", now_stamp()));
    std::fs::write(&saved_to, &zip_bytes).map_err(internal)?;
    owo_agent_server::logging::info(
        "storage",
        None,
        &format!("数据备份完成：{} 字节", zip_bytes.len()),
    );
    Ok(Json(json!({
        "ok": true,
        "archive_b64": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &zip_bytes),
        "size_bytes": zip_bytes.len(),
        "saved_to": saved_to.to_string_lossy(),
    })))
}

#[derive(serde::Deserialize)]
struct RestoreRequest {
    #[serde(default)]
    archive_b64: String,
}

async fn restore_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RestoreRequest>,
) -> Result<Json<Value>, (axum::http::StatusCode, String)> {
    if request.archive_b64.is_empty() {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            "缺少 archive_b64（先用 POST /storage/backup 生成备份）".to_string(),
        ));
    }
    let bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        &request.archive_b64,
    )
    .map_err(|e| {
        (
            axum::http::StatusCode::BAD_REQUEST,
            format!("archive_b64 解码失败：{e}"),
        )
    })?;
    // 恢复前先自动备份。
    let backup_dir = backups_dir(&state.data_root);
    std::fs::create_dir_all(&backup_dir).map_err(internal)?;
    let pre_backup = backup_dir.join(format!("pre-restore-{}.zip", now_stamp()));
    let pre_bytes = build_backup_zip(&state.data_root, &state.workspace).map_err(internal)?;
    std::fs::write(&pre_backup, &pre_bytes).map_err(internal)?;

    let (manifest, entries) = parse_restore_zip(&bytes).map_err(bad_request)?;
    let mut restored: Vec<String> = Vec::new();
    let mut staged: Vec<String> = Vec::new();
    let mut restart_required = false;
    for (safe, content) in entries {
        match safe.as_str() {
            "index.db" | "index.db-wal" | "index.db-shm" => {
                // 数据库文件：校验后暂存（服务持有打开句柄，重启后生效）。
                if safe == "index.db" {
                    let check = validate_sqlite(&content).map_err(bad_request)?;
                    if check != "ok" {
                        return Err(bad_request(format!(
                            "备份中的 index.db 完整性校验失败：{check}"
                        )));
                    }
                }
                let target = state.data_root.join(format!("{}.restored", safe));
                std::fs::write(&target, &content).map_err(internal)?;
                staged.push(safe.clone());
                restart_required = true;
            }
            "settings.json" => {
                let target = state.workspace.join("settings.json");
                std::fs::write(&target, &content).map_err(internal)?;
                restored.push(safe);
            }
            "memory.jsonl" | "plugin_state.json" | "automations.json" => {
                write_staged(&state.data_root, &safe, &content).map_err(internal)?;
                restored.push(safe);
            }
            _ => {
                let relative = safe
                    .strip_prefix("notes/")
                    .or_else(|| safe.strip_prefix("skills/"))
                    .or_else(|| safe.strip_prefix("goals/"))
                    .or_else(|| safe.strip_prefix("intent-workflows/"))
                    .or_else(|| safe.strip_prefix("eval/reports/"));
                let Some(relative) = relative else {
                    if let Some(workflow) = safe.strip_prefix("workflows/") {
                        let target = state.workspace.join(workflow);
                        if let Some(parent) = target.parent() {
                            std::fs::create_dir_all(parent).map_err(internal)?;
                        }
                        std::fs::write(&target, &content).map_err(internal)?;
                        restored.push(safe);
                        continue;
                    }
                    return Err(bad_request(format!("备份包含未知条目：{safe}")));
                };
                let target = state.data_root.join(relative);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(internal)?;
                }
                std::fs::write(&target, &content).map_err(internal)?;
                restored.push(safe);
            }
        }
    }
    let kind = manifest
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    Ok(Json(json!({
        "ok": true,
        "kind": kind,
        "restored": restored,
        "staged": staged,
        "restart_required": restart_required,
        "pre_backup": pre_backup.to_string_lossy(),
        "note": if restart_required { "index.db 已校验并暂存为 .restored，重启服务后生效（恢复前自动备份见 pre_backup）" } else { "恢复完成（恢复前自动备份见 pre_backup）" },
    })))
}

async fn export_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (axum::http::StatusCode, String)> {
    // sessions（全量）
    let mut sessions: Vec<Value> = Vec::new();
    for id in state.store.list() {
        if let Ok(session) = state.store.load(&id) {
            sessions.push(json!({
                "id": session.id,
                "workspace": session.workspace.to_string_lossy(),
                "model": session.model,
                "created_at": session.created_at,
                "updated_at": session.updated_at,
                "title": session.title,
                "archived": session.archived,
                "pinned": session.pinned,
                "parent_id": session.parent_id,
                "messages": session.messages,
            }));
        }
    }
    // 审计（全量）
    let (audit, _) = state
        .store
        .query_audit(&owo_agent_core::sqlite_store::AuditQuery {
            limit: usize::MAX,
            ..Default::default()
        });
    // notes（<data_root>/notes/<id>/doc.json）
    let mut notes_out: Vec<Value> = Vec::new();
    let notes_dir = state.data_root.join("notes");
    if let Ok(entries) = std::fs::read_dir(&notes_dir) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if dir.is_dir() {
                if let Ok(doc) = notes::load_doc(&dir) {
                    notes_out.push(json!({
                        "id": doc.id,
                        "title": doc.title,
                        "blocks": doc.blocks,
                    }));
                }
            }
        }
    }
    // skills（<data_root>/skills/**，文本文件）
    let mut skill_files: Vec<(String, String)> = Vec::new();
    let skills_root = state.data_root.join("skills");
    if skills_root.is_dir() {
        let mut stack = vec![(skills_root.clone(), "skills".to_string())];
        while let Some((current, prefix)) = stack.pop() {
            if let Ok(entries) = std::fs::read_dir(&current) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().to_string();
                    let relative = format!("{prefix}/{name}");
                    if path.is_dir() {
                        stack.push((path, relative));
                    } else if let Ok(content) = std::fs::read_to_string(&path) {
                        if content.len() < 2 * 1024 * 1024 {
                            skill_files.push((relative, content));
                        }
                    }
                }
            }
        }
    }
    // workflows（workspace/**/*.owflow）
    let mut workflows: Vec<Value> = Vec::new();
    let mut stack = vec![state.workspace.clone()];
    while let Some(current) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&current) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().and_then(|ext| ext.to_str()) == Some("owflow") {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        workflows.push(json!({
                            "path": path.strip_prefix(&state.workspace).map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
                            "content": content,
                        }));
                    }
                }
            }
        }
    }
    // settings
    let settings_path = state.workspace.join("settings.json");
    let settings: Value = std::fs::read_to_string(&settings_path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or(json!({}));
    Ok(Json(json!({
        "format_version": 1,
        "exported_at": chrono::Utc::now().to_rfc3339(),
        "counts": {
            "sessions": sessions.len(),
            "audit": audit.len(),
            "notes": notes_out.len(),
            "skills": skill_files.len(),
            "workflows": workflows.len(),
        },
        "sessions": sessions,
        "audit": audit,
        "notes": notes_out,
        "skills": skill_files.into_iter().map(|(path, content)| json!({ "path": path, "content": content })).collect::<Vec<_>>(),
        "workflows": workflows,
        "settings": settings,
    })))
}

#[derive(serde::Deserialize)]
struct ClearRequest {
    confirm: Option<String>,
}

async fn clear_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ClearRequest>,
) -> Result<Json<Value>, (axum::http::StatusCode, String)> {
    if request.confirm.as_deref() != Some(CLEAR_CONFIRM) {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            format!("二次确认失败：请携带 {{ \"confirm\": \"{CLEAR_CONFIRM}\" }}"),
        ));
    }
    // 1) 会话 + 审计（存储层清空）。
    state.store.clear().map_err(|e| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("清空存储失败：{e}"),
        )
    })?;
    // 2) 内存会话表与审计游标复位（防止旧内存数据回灌）。
    if let Ok(mut sessions) = state.sessions.lock() {
        sessions.clear();
    }
    if let Ok(mut flushed) = state.audit_flushed.lock() {
        *flushed = state
            .agent
            .audit_log()
            .lock()
            .map(|log| log.entries.len())
            .unwrap_or(0);
    }
    // 3) 文件型数据：notes / memory / automations。
    let mut cleared: Vec<String> = vec!["sessions".into(), "audit".into()];
    for name in ["notes", "goals", "intent-workflows"] {
        let dir = state.data_root.join(name);
        if dir.is_dir() {
            let _ = std::fs::remove_dir_all(&dir);
            cleared.push(name.to_string());
        }
    }
    for file in ["memory.jsonl", "memory.semantic.jsonl", "automations.json"] {
        let path = state.data_root.join(file);
        if path.is_file() {
            let _ = std::fs::remove_file(&path);
            cleared.push(file.to_string());
        }
    }
    // 4) 清空后完整性校验。
    let integrity = match state.store.recent_audit(10).is_empty() && state.store.list().is_empty() {
        true => "ok",
        false => "incomplete",
    };
    owo_agent_server::logging::warn(
        "storage",
        None,
        "一键清空完成",
        &[
            ("cleared", serde_json::json!(cleared)),
            ("integrity", serde_json::json!(integrity)),
        ],
    );
    Ok(Json(json!({
        "ok": true,
        "cleared": cleared,
        "integrity": integrity,
        "note": "会话/审计/笔记/记忆/自动化已清空；技能与工作流保留（属于配置资产）",
    })))
}

fn internal(message: impl std::fmt::Display) -> (axum::http::StatusCode, String) {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        message.to_string(),
    )
}

fn bad_request(message: impl std::fmt::Display) -> (axum::http::StatusCode, String) {
    (axum::http::StatusCode::BAD_REQUEST, message.to_string())
}
