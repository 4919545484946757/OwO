use crate::audit::AuditLog;
use crate::mcp::{McpClient, McpTool};
use crate::permissions::Policy;
use crate::session::Session;
use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

pub struct ToolContext<'a> {
    pub workspace: &'a Path,
    pub policy: &'a Policy,
    pub session: &'a mut Session,
    pub audit: &'a Arc<Mutex<AuditLog>>,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    async fn run(&self, ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String>;
}

pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        let mut registry = Self { tools: Vec::new() };
        registry.register(ReadFileTool);
        registry.register(WriteFileTool);
        registry.register(ListDirTool);
        registry.register(SearchFilesTool);
        registry.register(RunCommandTool);
        registry
    }

    pub fn register(&mut self, tool: impl Tool + 'static) {
        self.tools.push(Box::new(tool));
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|tool| tool.spec()).collect()
    }

    pub async fn execute(
        &self,
        name: &str,
        ctx: &mut ToolContext<'_>,
        args: Value,
    ) -> Result<Value, String> {
        let tool = self
            .tools
            .iter()
            .find(|tool| tool.spec().name == name)
            .ok_or_else(|| format!("未知工具：{name}"))?;
        tool.run(ctx, args).await
    }

    /// 把 MCP 服务器暴露的工具注册为 Agent 工具（命名：`{server}_{tool}`）。
    pub fn register_mcp_tools(
        &mut self,
        server_name: &str,
        client: Arc<tokio::sync::Mutex<McpClient>>,
        tools: Vec<McpTool>,
    ) {
        for tool in tools {
            let full_name = format!("{server_name}_{}", tool.name);
            let spec = ToolSpec {
                name: full_name.clone(),
                description: tool.description,
                input_schema: tool.input_schema,
            };
            self.tools.push(Box::new(McpToolAdapter {
                full_name,
                server_name: server_name.to_string(),
                tool_name: tool.name,
                spec,
                client: Arc::clone(&client),
            }));
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn required_string(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("参数缺少字符串字段：{key}"))
}

fn snapshot_key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

struct ReadFileTool;

#[async_trait]
impl Tool for ReadFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".into(),
            description: "读取工作区内的文本文件内容".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        }
    }

    async fn run(&self, ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let path = required_string(&args, "path")?;
        let abs = ctx.policy.resolve_within_workspace(&path)?;
        let content = tokio::fs::read_to_string(&abs)
            .await
            .map_err(|e| format!("读取 {path} 失败：{e}"))?;
        Ok(json!({
            "path": path,
            "content": content,
            "bytes": content.len(),
        }))
    }
}

struct WriteFileTool;

#[async_trait]
impl Tool for WriteFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_file".into(),
            description: "写入工作区内的文件（自动快照，可 diff/revert）".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn run(&self, ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let path = required_string(&args, "path")?;
        let content = required_string(&args, "content")?;
        let abs = ctx.policy.resolve_within_workspace(&path)?;
        let key = snapshot_key(&abs);
        if let std::collections::hash_map::Entry::Vacant(entry) = ctx.session.snapshots.entry(key) {
            let original = match tokio::fs::read(&abs).await {
                Ok(bytes) => Some(BASE64.encode(bytes)),
                Err(_) => None,
            };
            entry.insert(crate::session::SnapshotEntry {
                original_b64: original,
            });
        }
        if let Some(parent) = abs.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("创建目录失败：{e}"))?;
        }
        tokio::fs::write(&abs, content.as_bytes())
            .await
            .map_err(|e| format!("写入 {path} 失败：{e}"))?;
        Ok(json!({
            "path": path,
            "written": true,
            "bytes": content.len(),
        }))
    }
}

struct ListDirTool;

#[async_trait]
impl Tool for ListDirTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_dir".into(),
            description: "列出工作区内目录条目".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } }
            }),
        }
    }

    async fn run(&self, ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| ".".to_string());
        let abs = ctx.policy.resolve_within_workspace(&path)?;
        let mut entries = Vec::new();
        let mut reader = tokio::fs::read_dir(&abs)
            .await
            .map_err(|e| format!("读取目录 {path} 失败：{e}"))?;
        while let Some(entry) = reader.next_entry().await.map_err(|e| e.to_string())? {
            entries.push(json!({
                "name": entry.file_name().to_string_lossy(),
                "is_dir": entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false),
            }));
        }
        Ok(json!({ "path": path, "entries": entries }))
    }
}

struct SearchFilesTool;

#[async_trait]
impl Tool for SearchFilesTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "search_files".into(),
            description: "按文件名关键字递归搜索工作区文件".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "pattern": { "type": "string" } },
                "required": ["pattern"]
            }),
        }
    }

    async fn run(&self, ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let pattern = required_string(&args, "pattern")?.to_lowercase();
        let workspace = ctx.workspace.to_path_buf();
        let mut matches = Vec::new();
        collect_matches(&workspace, &workspace, &pattern, 0, &mut matches)
            .map_err(|e| format!("搜索失败：{e}"))?;
        Ok(json!({ "pattern": pattern, "matches": matches }))
    }
}

fn collect_matches(
    root: &Path,
    dir: &Path,
    pattern: &str,
    depth: usize,
    matches: &mut Vec<String>,
) -> std::io::Result<()> {
    if depth > 8 || matches.len() >= 200 {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_matches(root, &entry.path(), pattern, depth + 1, matches)?;
        } else if entry
            .file_name()
            .to_string_lossy()
            .to_lowercase()
            .contains(pattern)
        {
            let rel = entry
                .path()
                .strip_prefix(root)
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|_| entry.path());
            matches.push(rel.to_string_lossy().replace('\\', "/"));
        }
        if matches.len() >= 200 {
            break;
        }
    }
    Ok(())
}

struct RunCommandTool;

#[async_trait]
impl Tool for RunCommandTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "run_command".into(),
            description: "在工作区内执行 shell 命令（需审批，60 秒超时）".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "cwd": { "type": "string" }
                },
                "required": ["command"]
            }),
        }
    }

    async fn run(&self, ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let command = required_string(&args, "command")?;
        let cwd = args
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::to_string)
            .map(|path| ctx.policy.resolve_within_workspace(&path))
            .transpose()?
            .unwrap_or_else(|| ctx.workspace.to_path_buf());

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            tokio::process::Command::new("cmd")
                .arg("/C")
                .arg(&command)
                .current_dir(&cwd)
                .output(),
        )
        .await
        .map_err(|_| "命令执行超时（60s）".to_string())?
        .map_err(|e| format!("命令执行失败：{e}"))?;

        Ok(json!({
            "command": command,
            "exit_code": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
        }))
    }
}

struct McpToolAdapter {
    full_name: String,
    server_name: String,
    tool_name: String,
    spec: ToolSpec,
    client: Arc<tokio::sync::Mutex<McpClient>>,
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    async fn run(&self, _ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let mut client = self.client.lock().await;
        client
            .call_tool(&self.tool_name, args)
            .await
            .map_err(|error| {
                format!(
                    "MCP 工具 {}:{} 失败：{error}",
                    self.server_name, self.tool_name
                )
            })
    }
}

impl std::fmt::Debug for McpToolAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpToolAdapter")
            .field("full_name", &self.full_name)
            .finish()
    }
}
