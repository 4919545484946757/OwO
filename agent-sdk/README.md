# OwO Agent SDK

Codex 式 Agent 智能体 SDK（v0.1 骨架，M1 最小闭环）。

## 目标形态

参照 OpenAI Codex / OpenCode / Claude Code 的 Harness 架构，提供：

- Rust 核心库（`owo-agent-core`）：Agent loop、工具注册表、权限审批、会话、审计。
- HTTP API（`owo-agent-server`）：session/turn/permission/diff/revert/abort，SSE 事件流。
- CLI（`owo-agent-cli`）：交互式 `turn` 与 `serve`。

技术基线见 `../builGoal/技术文档-AI智能体输入法.md`（v0.3，输入法路线不实施）。

## 环境变量

```text
OPENAI_API_KEY=<你的 API Key>
OPENAI_BASE_URL=https://api.openai.com/v1      # 可指向 Ollama/兼容代理
OPENAI_MODEL=gpt-5.1-codex                      # 可替换任意 OpenAI-compatible 模型
OWO_AGENT_DATA=%LOCALAPPDATA%\OwO\Agent         # 会话/审计数据目录（可选）
```

## 构建与测试

```powershell
cargo build --workspace
cargo test --workspace
```

## CLI（OpenCode 式交互终端）

**全屏 TUI**（OpenCode 风格，推荐）：

```powershell
cargo run -p owo-agent-cli -- tui --workspace .
```

TUI 特性：标题栏（工作区/模型/模式/运行状态）、滚动会话区、输入框、快捷键提示；Tab 切换 build/plan、内联审批（y/n）、Ctrl+C 中止/退出、PgUp/PgDn 滚动、Ctrl+L 清屏。

**交互式 REPL**（文本终端/脚本）：

```powershell
cargo run -p owo-agent-cli -- repl --workspace .
```

交互终端支持：

- 直接输入文字发起任务；自动创建/恢复会话。
- `build` / `plan` 两种模式：plan 为只读（写/执行一律拒绝），build 的写操作需审批。
- `/new`、`/sessions`、`/resume <id>` 会话管理。
- `/diff` 查看文件改动（快照级），`/undo` 回滚全部写操作（新建文件会被删除）。
- `/model <名称>` 切换模型，`/status`、`/permissions`、`/audit` 查看状态。
- `/init` 生成 AGENTS.md；`/abort` 中止当前回合；`/exit` 退出。
- `/mcp add <名称> <命令> [参数...]`、`/mcp list`、`/mcp remove <名称>` 管理 MCP 服务器（配置持久化在 `<data>/mcp-servers.json`）。
- 支持管道输入（脚本/自动化）与历史记录（`<data>/history.txt`）。

接入任意 stdio MCP 服务器示例：

```text
/mcp add files npx -y @modelcontextprotocol/server-filesystem C:\workspace
/mcp list
```

HTTP MCP 服务器示例：

```text
/mcp add remote http https://example.com/mcp
/mcp list
```

一次性任务与 HTTP 服务：

```powershell
cargo run -p owo-agent-cli -- turn --workspace . --prompt "给 parseConfig 补单元测试"
cargo run -p owo-agent-cli -- init --workspace .
cargo run -p owo-agent-cli -- serve --port 4096
```

## 当前范围（M1）

- Agent loop：模型调用 → 工具执行 → 结果回填 → 停止条件（最大轮数/超时/中止）。
- 内置工具：`read_file`、`write_file`（带快照）、`list_dir`、`search_files`、`run_command`。
- 权限策略：workspace 作用域路径校验、deny/ask/allow、命令危险模式 deny 优先。
- 审批：CLI 交互审批、程序化 Approver（服务器审批通道）。
- 会话：JSON 持久化、diff、revert（回滚写操作）。
- 审计：内存审计记录（事件、工具、审批、结果）。
- 模型网关：OpenAI-compatible chat completions（工具调用）。
- 模型流式输出：SSE token 增量实时上屏（REPL/TUI 打字机效果），工具调用片段流式组装。
- MCP 客户端：stdio 与 HTTP（streamable HTTP，JSON/SSE 响应）双传输，工具以 `{server}_{tool}` 命名注册进 Agent。
- 子代理：`explore`（只读调查，独立子会话）与 `subagent`（通用委派，完整工具需审批），深度限制 2 层，共享中止/审批。
- Skills：发现工作区 `.agents/skills/` 与 `<data>/skills/` 下的 SKILL.md（Agent Skills 开放标准），清单注入系统提示，`use_skill` 工具按需取用；仓库自带 `demo-summary` 示例技能。
- 上下文压缩：估算 token 超预算时用模型把旧历史压成摘要（保留最近 N 条），压缩事件上屏并审计；可用 `OWO_TOKEN_BUDGET` / `OWO_KEEP_RECENT` 调参。
- 会话 fork/redo：`/fork [消息序号]` 派生子会话（parent/fork_point 持久化）、`/rewind <条数>` 回退历史并撤销文件改动、`/redo` 恢复、`/tree` 查看会话树；HTTP 服务同步提供对应端点。
- 会话分享：`/share [html]` 导出自包含 Markdown/HTML 会话记录（`<data>/shares/`），HTTP 端点 `GET /session/{id}/export/{md|html}`。
- 交互式 CLI：build/plan 模式、会话、diff/undo、审批、审计、AGENTS.md 初始化。

## 尚未实现（M2+）

- AGENTS.md 已注入；MCP HTTP 传输、SQLite 存储尚未实现。
- 上下文压缩（仅截断）、SQLite 存储、云执行、沙箱 OS 隔离、traces/evals 平台。
