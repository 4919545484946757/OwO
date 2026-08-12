# OwO Agent SDK

Codex 式 Agent 智能体 SDK（v0.1 骨架，M1 最小闭环）。

## 目标形态

参照 OpenAI Codex / OpenCode / Claude Code 的 Harness 架构，提供：

- Rust 核心库（`owo-agent-core`）：Agent loop、工具注册表、权限审批、会话、审计。
- HTTP API（`owo-agent-server`）：session/turn/permission/diff/revert/abort，SSE 事件流。
- CLI（`owo-agent-cli`）：交互式 `turn` 与 `serve`。

技术基线见 `../builGoal/技术文档-AI智能体输入法.md`（v0.3，输入法路线不实施）。
迭代依据见 `../builGoal/技术文档-AI智能体输入法-v0.4续写计划.md`（桌面端 + 全域情景感知 + 操作学习）。

## v0.4 已落地（SDK 侧）

- 应用白名单（`whitelist.rs`）：生产力/聊天/游戏/其他分级，敏感类默认禁止操作与学习。
- 全域情景感知（`perception.rs`）：L0-L3 分层、情景快照、消息掩码、L2 截图环形缓冲（不落盘、用后即毁）、SSE 订阅。
- 操作学习（`learn.rs`）：示范录制（暂停/清空/敏感面熔断）、动作图、流程技能包（SKILL.md + graph.json + manifest.json，可校验/存取/删除）、主动建议（阈值/频控/静默）。
- 内置技能包（`skills/`）：documents / spreadsheets / pdf / browser，遵循 SKILL.md + manifest.json + tests/ 契约，启动时自动安装到数据目录。
- HTTP 新接口：`GET /context/snapshot`、`GET /perception/events`（SSE）、`POST /learn/record|pause|resume|clear`、`GET /learn/status`、`POST /skill/verify`、`POST /proactive/observe|decide`、`GET /whitelist`、`POST /whitelist/manage`。
- 设置组：`stt` / `explore` / `proactive` / `skills` / `whitelist`（参考 `settings.example.json`）。
- 桌面工作台 Web 壳（P1 骨架）：`desktop/web/`（任务列表/对话 SSE 流式/审批条/diff 审阅/技能中心/感知状态区/白名单管理），由 HTTP 服务在 `/` 静态托管，后续用 Tauri 2 封装为桌面主客户端。
- L0 前台窗口事件源（Windows）：`platform.rs` 用 Win32 轮询前台应用（app_id + 标题），`/context/snapshot` 自动刷新并去重写入情景快照。
- L0 剪贴板事件源：轮询 `GetClipboardSequenceNumber`，只记录“内容已变化”（掩码），不读取/不保存剪贴板内容。
- L2 按需截图：GDI `BitBlt` + `GetDIBits` 抓屏为内存 BMP，进环形缓冲（最多 5 帧、不落盘），任务结束 `discard_captures` 即毁；快照只暴露元数据（大小/时间），不暴露像素。
- L1 无障碍 UI 树（Windows UI Automation）：`accessibility.rs` 抓取前台窗口语义锚点（角色/名称/类名，按深度与节点数截断），写入情景快照 `ui_context.ui_tree`，内容未变化不重复记事件。
- L2 本地摘要（Windows OCR）：`ocr.rs` 用系统自带 Media.Ocr 对内存截图离线识别文字，摘要进环形缓冲帧元数据（不落盘）；`POST /perception/capture` 按需采集（可传 width/height 采样），`POST /perception/layers` 逐层授权/热撤（L2 默认关闭，拒绝时 400）。

### 内置技能门禁

四个内置技能包都带可执行端到端契约测试（`skills/<name>/tests/run_tests.*`），
使用 python-docx / openpyxl / reportlab / pypdf / Poppler / Playwright + 本机 Edge：

```powershell
powershell -ExecutionPolicy Bypass -File scripts\skill-gate.ps1
# 或在 eval 门禁后追加技能门禁
powershell -ExecutionPolicy Bypass -File scripts\run-eval-gate.ps1 -Threshold 0.8 -SkillGate
```

工具链可用环境变量覆盖：`OWO_SKILL_PYTHON` / `OWO_SKILL_NODE` / `OWO_SKILL_PDFTOPPM` / `OWO_SKILL_RUNTIME`。

### 打开桌面工作台

```powershell
$env:OPENAI_API_KEY = "<你的 API Key>"
owo-agent serve --port 4096 --workspace .
# 浏览器打开 http://127.0.0.1:4096/
```

### 桌面主客户端（Tauri 2 壳）

```powershell
cargo build -p owo-agent-cli                    # 先编译核心服务
cd desktop\tauri\src-tauri
cargo build
.\target\debug\owo-agent-desktop.exe            # 自动拉起核心服务，Ctrl+Alt+Shift+O 唤起
```

Tauri 壳为无状态窗口：加载 `desktop/web/` 工作台，启动时自动拉起
`owo-agent serve`（127.0.0.1:4096），退出时回收子进程；含系统托盘（显示/退出）。
核心服务已开启 CORS（本机回环），供 WebView 跨源访问。

## 环境变量

```text
OPENAI_API_KEY=<你的 API Key>
OPENAI_BASE_URL=https://api.openai.com/v1      # 可指向 Ollama/兼容代理
OPENAI_MODEL=deepseek-v4-flash                  # 可替换任意 OpenAI-compatible 模型
OWO_AGENT_DATA=%LOCALAPPDATA%\OwO\Agent         # 会话/审计数据目录（可选）
```

工作区配置（`settings.json`，参考 `settings.example.json`）：默认模型、默认只读模式、额外危险命令片段、自动连接的 MCP 服务器、TUI 主题（dark/light）与键位（如 `toggle_mode`、`abort`、`scroll_up`、`clear`）；优先级为 命令行参数 > 环境变量 > settings.json > 内置默认。TUI 内可用 `/theme`、`/keybinds` 查看/切换。

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
- 消息级撤销/重做：`/undo-msg [n]` 移除最近 n 条对话消息，`/redo-msg` 恢复；与文件级 `/undo` 相互独立。
- 会话分享：`/share [html]` 导出自包含 Markdown/HTML 会话记录（`<data>/shares/`），HTTP 端点 `GET /session/{id}/export/{md|html}`。
- SQLite 存储：会话默认持久化到 `<data>/index.db`（rusqlite bundled），跨进程可恢复；JSON 存储保留用于测试/兼容。
- Evals：内置 5 用例演示套件（读/写/列目录/搜索/子代理），临时工作区隔离，输出成功率/耗时报告；`owo-agent eval` 与 HTTP `POST /eval/run`。
- Traces：每回合结构化轨迹（模型调用/流式 token/工具/审批/压缩/最终文本 + 耗时）自动写入 `<data>/traces/`，`/traces` 与 `/trace <n>` 回放，服务端回合同样落盘。
- CI 评估门禁：`scripts/run-eval-gate.ps1 [-Suite <json>] [-Threshold 0.8]` 运行 eval 并按通过率阈值退出 0/1。
- 本地插件：`plugins/<id>/manifest.json`（id/name/version/permissions/mcp）自动发现并桥接 MCP 工具（工作区 `plugins/` 优先于 `<data>/plugins/`），`/plugins` 查看；工具名自动净化以兼容模型 API 约束。
- 交互式 CLI：build/plan 模式、会话、diff/undo、审批、审计、AGENTS.md 初始化。

## 尚未实现（M2+）

- AGENTS.md 已注入；审计/用量入库（FTS5/向量索引）、桌面工作台、云执行为后续阶段。
- 上下文压缩（仅截断）、SQLite 存储、云执行、沙箱 OS 隔离、traces/evals 平台。
