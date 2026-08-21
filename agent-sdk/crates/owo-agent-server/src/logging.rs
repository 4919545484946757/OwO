// R11:logging 质量收尾完成。
// R12:logging 复核完成（trace_id 贯穿/脱敏，无需改动）。
//! 结构化日志与 trace_id（R8 + R9 + R10 文件日志/轮转）：JSON 单行日志 + 统一脱敏 + 请求贯穿 ID。
//! R10:logging 完成，待主控接线（`init_file_logging` 可选落盘 + 大小轮转；
//! `set_current_trace_id` 请求入口设置/清除；emit 未显式传 trace_id 时自动继承
//! 全局上下文；`audit_event` 供审计可观测面日志）。
//!
//! - `emit`：分级（trace/debug/info/warn/error）单行 JSON（ts/level/target/trace_id/msg/fields），
//!   同时写 stderr 与（可选）轮转文件。
//! - `Redactor`：统一脱敏器——apiKey/消息内容默认不落详文（保留前缀 + 哈希指纹）。
//! - `TraceId`：`X-Trace-Id` 头继承或生成（uuid 短格式），可序列化进日志与 SSE 帧。
//! - R9 全局 trace 上下文：`set_current_trace_id`/`current_trace_id`，供 SSE 事件、
//!   /metrics 关联字段与后台任务继承当前请求的 trace_id。
//! - R10 文件日志：`init_file_logging(path, max_bytes, backups)` 追加落盘，达到
//!   max_bytes 时按 `.1/.2/…` 轮转，保留 `backups` 份。
//!
//! 本模块不引用 `crate::`/`super::`，可被测试以 `#[path] mod` 独立编译。

// 主控收尾接线说明：lib 目标仅引用 TraceId/emit/Level；Redactor/safe_field/
// sanitize_json 属“测试面符号”，由测试以 #[path] 独立编译使用。
// 同 event_stream.rs 模块级 allow 做法。
#![allow(dead_code)]

use serde_json::{json, Value};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 日志级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    pub fn as_str(&self) -> &'static str {
        match self {
            Level::Trace => "trace",
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

/// 统一脱敏器：凭据/消息内容默认不落详文。
pub struct Redactor;

impl Redactor {
    /// apiKey 类：保留前 4 后 4，中间 `***`；短值整体掩码。按字符切片（避免多字节 UTF-8 panic）。
    pub fn redact_api_key(value: &str) -> String {
        let chars: Vec<char> = value.chars().collect();
        if chars.len() <= 8 {
            return "***".to_string();
        }
        let head: String = chars[..4].iter().collect();
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("{head}***{tail}")
    }

    /// 消息内容：只落长度 + 哈希指纹（DefaultHasher 前 8 位十六进制），不落详文。
    pub fn redact_message(value: &str) -> String {
        let fingerprint = {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut hasher = DefaultHasher::new();
            value.hash(&mut hasher);
            format!("{:016x}", hasher.finish())
        };
        format!("len={} hash={}", value.chars().count(), &fingerprint[..8])
    }

    /// 通用脱敏：非敏感字段可保留原文；此处默认同消息策略（保守）。
    pub fn redact(value: &str) -> String {
        Self::redact_message(value)
    }

    /// 按字段名选择策略（含 "key"/"token"/"secret"/"password"/"auth" 的字段名走密钥策略；
    /// "message"/"content"/"prompt"/"text" 走消息策略）。
    pub fn redact_field(name: &str, value: &str) -> String {
        let lower = name.to_ascii_lowercase();
        if [
            "key", "token", "secret", "password", "api_key", "apikey", "auth", "bearer",
        ]
        .iter()
        .any(|k| lower.contains(k))
        {
            Self::redact_api_key(value)
        } else if ["message", "content", "prompt", "text", "body"]
            .iter()
            .any(|k| lower.contains(k))
        {
            Self::redact_message(value)
        } else {
            Self::redact(value)
        }
    }
}

/// 请求贯穿 ID：从 `X-Trace-Id` 头继承或生成。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceId(pub String);

impl TraceId {
    /// 生成新 trace_id（uuid v4 短格式）。
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string()[..24].to_string())
    }

    /// 从请求头继承（非空合法）或生成。
    pub fn from_header(header: Option<&str>) -> Self {
        match header {
            Some(v)
                if !v.trim().is_empty()
                    && v.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') =>
            {
                Self(v.trim().to_string())
            }
            _ => Self::generate(),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 回填响应头 `X-Trace-Id` 的值。
    pub fn to_header_value(&self) -> String {
        self.0.clone()
    }
}

/// 全局 trace 上下文（R9）：请求入口设置，后台任务/SSE/指标可继承。
/// 单例语义为“当前活跃请求”；接线方在 middleware 中 set，请求结束清除。
static CURRENT_TRACE: Mutex<Option<String>> = Mutex::new(None);

/// 设置当前 trace 上下文（None 清除）。
pub fn set_current_trace_id(trace_id: Option<&str>) {
    let mut slot = CURRENT_TRACE.lock().unwrap_or_else(|e| e.into_inner());
    *slot = trace_id.map(str::to_string);
}

/// 读取当前 trace 上下文（无则 None）。
pub fn current_trace_id() -> Option<String> {
    CURRENT_TRACE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// 输出一条 JSON 结构化日志（单行，stderr + 可选文件；无 trace_id 时继承全局上下文，仍无则省略字段）。
pub fn emit(
    level: Level,
    target: &str,
    trace_id: Option<&str>,
    message: &str,
    fields: &[(&str, Value)],
) {
    let effective_trace = trace_id.map(str::to_string).or_else(current_trace_id);
    let mut entry = json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "level": level.as_str(),
        "target": target,
        "msg": message,
    });
    if let Some(trace_id) = effective_trace {
        entry["trace_id"] = json!(trace_id);
    }
    for (name, value) in fields {
        entry[name] = value.clone();
    }
    let line = entry.to_string();
    eprintln!("{}", line);
    write_file_log(&line);
}

// ==================== R10：文件日志与轮转 ====================

/// 文件日志配置（append + 大小轮转）。
struct FileLog {
    file: std::fs::File,
    path: PathBuf,
    max_bytes: u64,
    backups: u32,
    current: u64,
}

static FILE_LOG: Mutex<Option<FileLog>> = Mutex::new(None);

/// 初始化文件日志（R10）：追加写入 `path`，达到 `max_bytes` 时轮转
/// （`.1/.2/…` 移位），保留 `backups` 份（上限 9）。失败返回 Err，不 panic；
/// 未初始化时 emit 仅落 stderr。
pub fn init_file_logging(path: &Path, max_bytes: u64, backups: u32) -> std::io::Result<()> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let current = file.metadata().map(|m| m.len()).unwrap_or(0);
    *FILE_LOG.lock().unwrap_or_else(|e| e.into_inner()) = Some(FileLog {
        file,
        path: path.to_path_buf(),
        max_bytes: max_bytes.max(1024),
        backups: backups.clamp(0, 9),
        current,
    });
    Ok(())
}

/// 关闭文件日志（优雅关闭时调用）。
pub fn close_file_logging() {
    *FILE_LOG.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// 追加一行到轮转文件（失败静默——日志不得拖垮业务）。
fn write_file_log(line: &str) {
    let mut guard = FILE_LOG.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(file_log) = guard.as_mut() {
        let bytes = line.len() as u64 + 1;
        if file_log.current + bytes > file_log.max_bytes {
            rotate(file_log);
        }
        if file_log.current + bytes <= file_log.max_bytes {
            let _ = file_log.file.write_all(line.as_bytes());
            let _ = file_log.file.write_all(b"\n");
            file_log.current += bytes;
        }
    }
}

/// 轮转：`.n` 文件依次后移，当前文件移为 `.1`，重新打开新文件。
fn rotate(file_log: &mut FileLog) {
    let path = file_log.path.clone();
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let _ = file_log.file.flush();
    for i in (1..file_log.backups).rev() {
        let src = path.with_file_name(format!("{name}.{i}"));
        let dst = path.with_file_name(format!("{name}.{}", i + 1));
        if dst.exists() {
            let _ = std::fs::remove_file(&dst);
        }
        if src.exists() {
            let _ = std::fs::rename(&src, &dst);
        }
    }
    let first = path.with_file_name(format!("{name}.1"));
    if first.exists() {
        let _ = std::fs::remove_file(&first);
    }
    let _ = std::fs::rename(&path, &first);
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(file) => {
            file_log.file = file;
            file_log.current = 0;
        }
        Err(_) => {
            // 打开失败：标记已满，避免反复轮转。
            file_log.current = file_log.max_bytes;
        }
    }
}

/// 审计事件便捷（R10）：结构化 + trace_id 贯穿 + 详情强制脱敏。
/// 注：HMAC 审计链在 core `audit_chain`；此处为可观测面审计日志事件。
pub fn audit_event(action: &str, trace_id: Option<&str>, detail: &str) {
    emit(
        Level::Info,
        "audit",
        trace_id,
        "audit_event",
        &[
            ("action", json!(action)),
            ("detail", safe_field("detail", detail)),
        ],
    );
}

/// 便捷：info 级。
pub fn info(target: &str, trace_id: Option<&str>, message: &str) {
    emit(Level::Info, target, trace_id, message, &[]);
}

/// 便捷：warn 级。
pub fn warn(target: &str, trace_id: Option<&str>, message: &str, fields: &[(&str, Value)]) {
    emit(Level::Warn, target, trace_id, message, fields);
}

/// 便捷：error 级。
pub fn error(target: &str, trace_id: Option<&str>, message: &str, fields: &[(&str, Value)]) {
    emit(Level::Error, target, trace_id, message, fields);
}

/// 把字段值按脱敏策略转换（供调用方落日志前使用）。
pub fn safe_field(name: &str, value: &str) -> Value {
    json!(Redactor::redact_field(name, value))
}

/// 把任意 JSON 值中的敏感字段原地脱敏（遍历一层，递归两层）。
pub fn sanitize_json(value: &mut Value) {
    fn walk(value: &mut Value, depth: usize) {
        if depth > 2 {
            return;
        }
        if let Value::Object(map) = value {
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                if let Some(field) = map.get_mut(&key) {
                    if field.is_string() {
                        let original = field.as_str().unwrap_or_default();
                        *field = json!(Redactor::redact_field(&key, original));
                    } else {
                        walk(field, depth + 1);
                    }
                }
            }
        } else if let Value::Array(items) = value {
            for item in items {
                walk(item, depth + 1);
            }
        }
    }
    walk(value, 0);
}

/// 占位：确保 std::fmt::Write 路径类型完整（供测试独立编译）。
#[allow(dead_code)]
fn _probe_format() -> String {
    let mut s = String::new();
    let _ = write!(s, "{}", TraceId::generate().as_str());
    s
}
