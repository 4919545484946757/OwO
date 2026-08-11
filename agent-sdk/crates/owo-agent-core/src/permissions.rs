use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Read,
    Write,
    Execute,
    Inject,
}

impl Level {
    pub fn label(&self) -> &'static str {
        match self {
            Level::Read => "read",
            Level::Write => "write",
            Level::Execute => "execute",
            Level::Inject => "inject",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone, Serialize)]
pub struct PermissionRequest {
    pub request_id: String,
    pub tool: String,
    pub args: Value,
    pub level: Level,
    pub reason: String,
}

#[async_trait]
pub trait Approver: Send + Sync {
    async fn decide(&self, request: &PermissionRequest) -> Decision;
}

/// 测试/自动化用：统一放行或拒绝。
pub struct AutoApprover {
    pub allow: bool,
}

#[async_trait]
impl Approver for AutoApprover {
    async fn decide(&self, _request: &PermissionRequest) -> Decision {
        if self.allow {
            Decision::Allow
        } else {
            Decision::Deny
        }
    }
}

/// 权限策略：deny 优先，其次 allow 规则，最后 ask。
/// 作用域：所有文件/命令路径必须位于 workspace 内。
pub struct Policy {
    workspace: PathBuf,
    deny_command_fragments: Vec<String>,
}

impl Policy {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            deny_command_fragments: vec![
                "rm -rf".to_string(),
                "sudo".to_string(),
                "shutdown".to_string(),
                "format c:".to_string(),
                "rd /s".to_string(),
                "remove-item -recurse".to_string(),
                "del /s".to_string(),
                "git push".to_string(),
                "git reset --hard".to_string(),
            ],
        }
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub fn level_for(tool: &str) -> Level {
        match tool {
            "read_file" | "list_dir" | "search_files" => Level::Read,
            "write_file" => Level::Write,
            "run_command" => Level::Execute,
            "text.inject" | "clipboard" => Level::Inject,
            _ => Level::Execute,
        }
    }

    /// 解析并校验路径位于 workspace 内（文件可尚不存在，校验父级）。
    pub fn resolve_within_workspace(&self, path: &str) -> Result<PathBuf, String> {
        resolve_within(&self.workspace, path)
    }

    pub fn evaluate(&self, tool: &str, args: &Value) -> PermissionRequest {
        let request_id = uuid::Uuid::new_v4().to_string();
        let level = Self::level_for(tool);
        let reason = match tool {
            "read_file" | "write_file" => {
                let path = args.get("path").and_then(Value::as_str).unwrap_or_default();
                match self.resolve_within_workspace(path) {
                    Ok(_) => format!("{level} 文件操作（工作区内）", level = level.label()),
                    Err(e) => format!("拒绝：{e}"),
                }
            }
            "list_dir" | "search_files" => "目录/搜索操作".to_string(),
            "run_command" => {
                let command = args
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let lower = command.to_lowercase();
                if self
                    .deny_command_fragments
                    .iter()
                    .any(|frag| lower.contains(frag))
                {
                    "拒绝：命令命中危险模式".to_string()
                } else {
                    format!("执行命令：{command}")
                }
            }
            _ => format!("工具 {tool} 需要审批"),
        };
        PermissionRequest {
            request_id,
            tool: tool.to_string(),
            args: args.clone(),
            level,
            reason,
        }
    }

    /// 工具执行前的最终判定（拒绝原因通过 request.reason 表达）。
    pub fn decision(&self, request: &PermissionRequest) -> Decision {
        if request.reason.starts_with("拒绝") {
            return Decision::Deny;
        }
        match request.level {
            Level::Read => Decision::Allow,
            Level::Write | Level::Execute | Level::Inject => Decision::Ask,
        }
    }
}

pub fn resolve_within(workspace: &Path, path: &str) -> Result<PathBuf, String> {
    let raw = PathBuf::from(path);
    let candidate = if raw.is_absolute() {
        raw
    } else {
        workspace.join(raw)
    };
    let canonical_workspace = workspace
        .canonicalize()
        .map_err(|e| format!("工作区不可访问：{e}"))?;
    let canonical_candidate =
        canonicalize_existing_parent(&candidate).map_err(|e| format!("路径校验失败：{e}"))?;
    if !canonical_candidate.starts_with(&canonical_workspace) {
        return Err(format!("路径位于工作区之外：{path}"));
    }
    Ok(candidate)
}

fn canonicalize_existing_parent(path: &Path) -> std::io::Result<PathBuf> {
    if path.exists() {
        return path.canonicalize();
    }
    let mut current = path;
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if current.exists() {
            let mut base = current.canonicalize()?;
            for part in suffix.iter().rev() {
                base.push(part);
            }
            return Ok(base);
        }
        match current.parent() {
            Some(parent) => {
                if let Some(name) = current.file_name() {
                    suffix.push(name.to_os_string());
                }
                current = parent;
            }
            None => return Ok(path.to_path_buf()),
        }
    }
}
