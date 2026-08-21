use crate::audit::AuditLog;
use crate::mcp::{McpClient, McpTool};
use crate::permissions::Policy;
use crate::session::Session;
use crate::skill::SkillRegistry;
use crate::subagent::SubagentRunner;
use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
    pub subagent: Option<SubagentRunner<'a>>,
    pub skills: &'a SkillRegistry,
    /// 窗口元素注册表（感知多源融合的稳定元素 ID 空间）。
    pub elements: &'a Arc<Mutex<crate::ElementRegistry>>,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    async fn run(&self, ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String>;
}

pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
    /// MCP 大 schema 的完整副本（注册时超预算被压缩为骨架；此处保留原始 schema 供按需查询）。
    full_schemas: HashMap<String, Value>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            tools: Vec::new(),
            full_schemas: HashMap::new(),
        };
        registry.register(ReadFileTool);
        registry.register(WriteFileTool);
        registry.register(ListDirTool);
        registry.register(SearchFilesTool);
        registry.register(RunCommandTool);
        registry.register(ExploreTool);
        registry.register(SubagentTool);
        registry.register(UseSkillTool);
        registry.register(crate::computer_use::ScreenOcrTool);
        registry.register(crate::computer_use::OcrRegionTool);
        registry.register(crate::computer_use::DesktopWindowOcrTool);
        registry.register(crate::computer_use::DesktopForegroundTool);
        registry.register(crate::computer_use::DesktopWindowListTool);
        registry.register(crate::computer_use::DesktopActivateTool);
        registry.register(crate::computer_use::DesktopClickTool);
        registry.register(crate::computer_use::DesktopTypeTool);
        registry.register(crate::computer_use::DesktopKeyTool);
        registry.register(crate::computer_use::DesktopShortcutTool);
        registry.register(crate::computer_use::DesktopLaunchTool);
        registry.register(crate::computer_use::DesktopScrollTool);
        registry.register(crate::computer_use::DesktopWaitTool);
        registry.register(crate::computer_use::DesktopWaitUntilTool);
        registry.register(crate::computer_use::ScreenVisionTool);
        registry.register(crate::computer_use::VisionVerifyTool);
        registry.register(crate::computer_use::VisionGroundTool);
        let browser = crate::computer_use::BrowserTools::new();
        registry.register(crate::computer_use::BrowserNavigateTool {
            tools: browser.clone(),
        });
        registry.register(crate::computer_use::BrowserSearchTool {
            tools: browser.clone(),
        });
        registry.register(crate::computer_use::BrowserSnapshotTool {
            tools: browser.clone(),
        });
        registry.register(crate::computer_use::BrowserClickTool {
            tools: browser.clone(),
        });
        registry.register(crate::computer_use::BrowserTypeTool {
            tools: browser.clone(),
        });
        registry.register(crate::computer_use::BrowserPressTool {
            tools: browser.clone(),
        });
        registry.register(crate::computer_use::BrowserScreenshotWriteTool {
            tools: browser.clone(),
        });
        registry.register(crate::computer_use::BrowserDownloadImageWriteTool {
            tools: browser.clone(),
        });
        registry.register(crate::computer_use::BrowserCloseTool { tools: browser });
        registry
    }

    /// 只读工具表（子代理 explore 使用）：不含写/执行/委派工具。
    pub fn read_only() -> Self {
        let mut registry = Self {
            tools: Vec::new(),
            full_schemas: HashMap::new(),
        };
        registry.register(ReadFileTool);
        registry.register(ListDirTool);
        registry.register(SearchFilesTool);
        registry
    }

    pub fn register(&mut self, tool: impl Tool + 'static) {
        self.tools.push(Arc::new(tool));
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|tool| tool.spec()).collect()
    }

    /// 按前缀撤销工具（插件热卸载：`owo_plugin_<id>_` 前缀）。
    /// 返回被移除的工具数。
    pub fn remove_prefix(&mut self, prefix: &str) -> usize {
        self.remove_prefix_inner(prefix)
    }

    /// 取工具句柄（Arc 克隆，锁外可跨 await 执行）。
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools
            .iter()
            .find(|tool| tool.spec().name == name)
            .cloned()
    }

    pub async fn execute(
        &self,
        name: &str,
        ctx: &mut ToolContext<'_>,
        args: Value,
    ) -> Result<Value, String> {
        let tool = self.get(name).ok_or_else(|| format!("未知工具：{name}"))?;
        tool.run(ctx, args).await
    }

    /// 把 MCP 服务器暴露的工具注册为 Agent 工具（命名：`{server}_{tool}`）。
    ///
    /// 延迟加载（M2）：单工具 schema 超过 `schema_budget_bytes` 时，注册为模型可见的
    /// **压缩骨架**（仅保留 type/required/属性名+属性类型，剔除 description/enum/嵌套细节），
    /// 完整 schema 保留在 `full_schemas` 供 `full_schema()` 按需查询——大 schema 服务不
    /// 显著占用模型上下文；调用工具时仍以完整 schema 校验。
    pub fn register_mcp_tools(
        &mut self,
        server_name: &str,
        client: Arc<tokio::sync::Mutex<McpClient>>,
        tools: Vec<McpTool>,
    ) {
        let budget = schema_budget_bytes();
        for tool in tools {
            let full_name = format!(
                "{}_{}",
                sanitize_tool_name(server_name),
                sanitize_tool_name(&tool.name)
            );
            let (input_schema, full_schema) = if schema_bytes(&tool.input_schema) > budget {
                let full = tool.input_schema.clone();
                (compact_schema(&tool.input_schema), Some(full))
            } else {
                (tool.input_schema, None)
            };
            let mut description = tool.description;
            if full_schema.is_some() {
                description.push_str("（schema 已压缩，完整参数见 /mcp/schema 接口）");
            }
            let spec = ToolSpec {
                name: full_name.clone(),
                description,
                input_schema,
            };
            if let Some(full) = full_schema {
                self.full_schemas.insert(full_name.clone(), full);
            }
            self.tools.push(Arc::new(McpToolAdapter {
                full_name,
                server_name: server_name.to_string(),
                tool_name: tool.name,
                spec,
                client: Arc::clone(&client),
            }));
        }
    }

    /// 按需取 MCP 工具的完整 schema（压缩注册时保留；小 schema 工具不重复存储）。
    pub fn full_schema(&self, name: &str) -> Option<Value> {
        self.full_schemas.get(name).cloned()
    }

    /// 移除工具时同步清理完整 schema 副本。
    fn remove_prefix_inner(&mut self, prefix: &str) -> usize {
        let before = self.tools.len();
        self.tools
            .retain(|tool| !tool.spec().name.starts_with(prefix));
        self.full_schemas
            .retain(|name, _| !name.starts_with(prefix));
        before - self.tools.len()
    }
}

/// 工具名只允许字母数字、下划线与连字符（模型 API 约束）。
fn sanitize_tool_name(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

/// MCP 工具注册前缀（`{server}_{tool}` 命名空间）：如 `owo_plugin_owo-translate_`。
pub fn mcp_tool_prefix(server_name: &str) -> String {
    format!("{}_", sanitize_tool_name(server_name))
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn required_string(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("参数缺少字符串字段：{key}"))
}

fn snapshot_key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// MCP 工具 schema 延迟加载（M2）：单工具 schema 序列化字节数预算，默认 2048 字节。
pub fn schema_budget_bytes() -> usize {
    std::env::var("OWO_MCP_SCHEMA_BUDGET_BYTES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(2048)
}

/// 估算 JSON schema 的序列化体积（字节）。
pub fn schema_bytes(schema: &Value) -> usize {
    serde_json::to_string(schema)
        .map(|text| text.len())
        .unwrap_or(usize::MAX)
}

/// 把 JSON Schema 压缩为模型可见的骨架：仅保留 `type`、`required` 与
/// 属性名+属性类型（字符串属性类型；嵌套对象/数组仅保留层级 type）。
/// 剔除 description / enum / pattern / 嵌套细节，体积大幅缩小。
/// 非对象 schema 原样返回（按需加载不适用）。
pub fn compact_schema(schema: &Value) -> Value {
    let Value::Object(map) = schema else {
        return schema.clone();
    };
    let mut compact = serde_json::Map::new();
    if let Some(t) = map.get("type") {
        compact.insert("type".to_string(), t.clone());
    }
    if let Some(required) = map.get("required") {
        compact.insert("required".to_string(), required.clone());
    }
    if let Some(properties) = map.get("properties").and_then(Value::as_object) {
        let mut props = serde_json::Map::new();
        for (name, property) in properties {
            let mut item = serde_json::Map::new();
            if let Some(t) = property.get("type") {
                item.insert("type".to_string(), t.clone());
            }
            props.insert(name.clone(), Value::Object(item));
        }
        compact.insert("properties".to_string(), Value::Object(props));
    }
    Value::Object(compact)
}

/// 以会话工作区为基座解析相对路径，并做策略工作区越界检查。
pub(crate) fn resolve_session_path(ctx: &ToolContext, path: &str) -> Result<PathBuf, String> {
    let base = ctx
        .workspace
        .canonicalize()
        .map_err(|error| format!("工作区不可访问：{error}"))?;
    let candidate = base.join(path);
    let candidate = candidate.canonicalize().unwrap_or(candidate);
    let policy_workspace = ctx
        .policy
        .workspace()
        .canonicalize()
        .unwrap_or_else(|_| ctx.policy.workspace().to_path_buf());
    if !candidate.starts_with(&policy_workspace) {
        return Err(format!("路径越界：{path}"));
    }
    Ok(candidate)
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
        let abs = resolve_session_path(ctx, &path)?;
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
        let abs = resolve_session_path(ctx, &path)?;
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
        let abs = resolve_session_path(ctx, &path)?;
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
            .map(|path| resolve_session_path(ctx, &path))
            .transpose()?
            .unwrap_or_else(|| ctx.workspace.to_path_buf());

        // 沙箱门卫：run_command 统一经 SandboxManager 执行（X01）。
        // 策略：工作区作用域 + 危险片段 deny + Job 级隔离（允许显式降级，审计记录）。
        let mut policy = crate::sandbox::SandboxPolicy::for_workspace("run_command", ctx.workspace);
        policy.require_isolation = crate::sandbox::IsolationLevel::JobOnly;
        policy.allow_degraded = true;
        policy.cpu_ms = Some(60_000);
        policy.mem_mb = Some(1024);
        // 命令文本（cmd /C <command> 的命令体）同样过 deny 检查。
        if let Some(fragment) =
            crate::sandbox::SandboxCommand::deny_hit(&command, &policy.deny_programs)
        {
            return Err(format!("命令命中危险黑名单片段：{fragment}"));
        }
        let sandbox_command = crate::sandbox::SandboxCommand::new("cmd", policy.clone())
            .with_args(vec!["/C".to_string(), command.to_string()])
            .with_cwd(cwd.clone());

        let manager = crate::sandbox::default_manager();
        let process = {
            let mut manager = manager
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            manager
                .spawn(&sandbox_command)
                .map_err(|error| format!("沙箱拒绝执行（{}）：{error}", command))?
        };

        // 同步等待放在 blocking 线程；超时仅报错，进程仍在 Job 内受限（CPU/内存上限兜底）。
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            tokio::task::spawn_blocking(move || {
                let mut process = process;
                process.wait_output()
            }),
        )
        .await
        .map_err(|_| "命令执行超时（60s，进程仍在受限 Job 内，将被资源上限终止）".to_string())?
        .map_err(|join_error| format!("命令等待失败：{join_error}"))?
        .map_err(|error| format!("沙箱执行失败：{error}"))?;

        Ok(json!({
            "command": command,
            "exit_code": output.exit_code,
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

struct ExploreTool;

#[async_trait]
impl Tool for ExploreTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "explore".into(),
            description: "把调查任务交给只读探索子代理（只能读/搜文件），返回其调查汇报".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"]
            }),
        }
    }

    async fn run(&self, ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or("参数缺少字符串字段：query")?;
        let runner = ctx.subagent.as_ref().ok_or("子代理运行时不可用")?;
        let text = runner.run(ctx.workspace, query, true).await?;
        Ok(json!({ "mode": "explore", "text": text }))
    }
}

struct SubagentTool;

#[async_trait]
impl Tool for SubagentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "subagent".into(),
            description: "把独立任务委派给通用子代理（完整工具、仍需审批），返回其汇报".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "task": { "type": "string" } },
                "required": ["task"]
            }),
        }
    }

    async fn run(&self, ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let task = args
            .get("task")
            .and_then(Value::as_str)
            .ok_or("参数缺少字符串字段：task")?;
        let runner = ctx.subagent.as_ref().ok_or("子代理运行时不可用")?;
        let text = runner.run(ctx.workspace, task, false).await?;
        Ok(json!({ "mode": "general", "text": text }))
    }
}

struct UseSkillTool;

#[async_trait]
impl Tool for UseSkillTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "use_skill".into(),
            description: "读取已加载技能（SKILL.md）的完整指令并按其流程执行；名称可通过 /skills 或技能清单查看".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "task": { "type": "string" }
                },
                "required": ["name"]
            }),
        }
    }

    async fn run(&self, ctx: &mut ToolContext<'_>, args: Value) -> Result<Value, String> {
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .ok_or("参数缺少字符串字段：name")?;
        let Some(skill) = ctx.skills.get_enabled(name) else {
            let available = ctx
                .skills
                .list_enabled()
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "未找到技能或技能已禁用：{name}；可用技能：{available}"
            ));
        };
        let task = args.get("task").and_then(Value::as_str).unwrap_or_default();
        Ok(json!({
            "skill": skill.name,
            "description": skill.description,
            "task": task,
            "instructions": skill.instructions,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_tool_names_for_model_api() {
        assert_eq!(
            sanitize_tool_name("owo.plugin.example-hello"),
            "owo_plugin_example-hello"
        );
        assert_eq!(sanitize_tool_name("echo"), "echo");
        assert_eq!(sanitize_tool_name("a b/c"), "a_b_c");
    }

    #[test]
    fn mcp_tool_prefix_sanitizes_plugin_id() {
        assert_eq!(
            mcp_tool_prefix("owo.plugin.translate"),
            "owo_plugin_translate_"
        );
        assert_eq!(
            mcp_tool_prefix("owo-plugin-clipboard"),
            "owo-plugin-clipboard_"
        );
    }

    struct NamedTool {
        name: String,
    }

    #[async_trait]
    impl Tool for NamedTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name.clone(),
                description: String::new(),
                input_schema: serde_json::json!({}),
            }
        }

        async fn run(
            &self,
            _ctx: &mut ToolContext<'_>,
            _args: serde_json::Value,
        ) -> Result<serde_json::Value, String> {
            Ok(serde_json::Value::Null)
        }
    }

    #[test]
    fn remove_prefix_unregisters_only_matching_tools() {
        let mut registry = ToolRegistry::new();
        registry.register(NamedTool {
            name: "owo_plugin_demo_translate".to_string(),
        });
        registry.register(NamedTool {
            name: "owo_plugin_demo_clipboard".to_string(),
        });
        registry.register(NamedTool {
            name: "builtin_tool".to_string(),
        });
        let removed = registry.remove_prefix("owo_plugin_demo_");
        assert_eq!(removed, 2);
        let names: Vec<String> = registry
            .specs()
            .iter()
            .map(|spec| spec.name.clone())
            .collect();
        assert!(!names
            .iter()
            .any(|name| name.starts_with("owo_plugin_demo_")));
        assert!(names.iter().any(|name| name == "builtin_tool"));
    }
}
