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

直接运行 `owo-agent repl`（或 `owo-agent`）进入交互式终端：

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
- 支持管道输入（脚本/自动化）与历史记录（`<data>/history.txt`）。

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
- 交互式 CLI：build/plan 模式、会话、diff/undo、审批、审计、AGENTS.md 初始化。

## 尚未实现（M2+）

- AGENTS.md 已注入；Skills/子代理/MCP 尚未实现。
- 上下文压缩（仅截断）、SQLite 存储、云执行、沙箱 OS 隔离、traces/evals 平台。
