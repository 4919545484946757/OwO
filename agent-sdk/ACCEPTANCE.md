# OwO Agent CLI 验收盘点

> 日期：2026-08-11 ｜ 分支：agent-frame ｜ 基线：技术文档 v0.3（仅实施 Agent 智能体方案）

本文档把“OpenCode 式 CLI 完整做出来”的目标拆成可审计清单：每项给出实现位置、验证命令与已完成的实测证据。

## 一、功能清单（对标 OpenCode）

| OpenCode 能力 | 实现 | 证据/命令 |
|---|---|---|
| 全屏 TUI（多面板/滚动/流式/审批/主题/键位/差异视图） | `crates/owo-agent-cli/src/tui.rs` | `owo-agent tui`；`/diff`（d 切换）、`/theme`、`/keybinds` |
| 交互式 REPL 与管道模式 | `main.rs`（`Repl`） | `owo-agent repl`；管道输入支持共享 stdin |
| 会话管理（new/sessions/resume/fork/rewind/redo/tree/undo-msg） | `core/src/session.rs`、HTTP 端点 | `owo-agent repl` 内 `/sessions`、`/fork`、`/rewind`、`/redo`、`/tree`、`/undo-msg` |
| 文件 diff/undo（快照回滚，新建文件可删） | `core/src/session.rs`、`tools.rs` | `/diff`、`/undo`；测试 `revert_removes_created_file` |
| 权限审批（deny/ask/allow、危险命令 deny） | `core/src/permissions.rs` | 实测：写文件/工具调用弹审批，越权被拒并审计 |
| 流式输出（SSE token 增量 + 工具调用片段组装） | `core/src/gateway.rs` | 实测 DeepSeek 打字机输出；测试 `streaming_deltas_are_emitted...` |
| MCP stdio + HTTP 双传输 | `core/src/mcp.rs` | `/mcp add <name> <cmd>` / `/mcp add <name> http <url>`；测试 stdio+HTTP |
| 子代理（explore/subagent + @直呼） | `core/src/subagent.rs`、`agent.rs` | `@explore <问题>`、`@subagent <任务>`；深度限制 2 层 |
| Skills（SKILL.md 发现/清单注入/use_skill） | `core/src/skill.rs` | 示例 `.agents/skills/demo-summary`；`/skills` |
| AGENTS.md 项目规则 | `core/src/context.rs` | 每次会话注入；仓库根 AGENTS.md |
| 上下文压缩（模型摘要 + 截断兜底 + 规则保留） | `core/src/agent.rs` | `OWO_TOKEN_BUDGET`/`OWO_KEEP_RECENT` 调参；测试断言 AGENTS.md 规则在压缩后仍注入 |
| Skills 热加载（不重启会话） | `core/src/skill.rs`、CLI | `/skills reload`；实测新增 SKILL.md 后 reload 立即可见 |
| /share（Markdown/HTML 导出 + HTTP） | `core/src/share.rs` | `/share [html]`；`GET /session/{id}/export/{md\|html}` |
| SQLite 存储（含老库迁移） | `core/src/sqlite_store.rs` | `<data>/index.db`；测试迁移与往返 |
| Evals（内置 20+ 用例套件 + 报告 + 门禁脚本） | `core/src/eval.rs`、`scripts/run-eval-gate.ps1` | `owo-agent eval`；测试 `builtin_suite_has_at_least_twenty_cases` |
| Traces（回合轨迹落盘/回放） | `core/src/trace.rs` | `/traces`、`/trace <n>`；实测含流式 token 事件 |
| 工作区配置 settings.json（模型/只读/deny/MCP/主题/键位） | `core/src/settings.rs` | `settings.example.json`；`/settings`、`/theme`、`/keybinds` |
| 本地插件 SDK（manifest + MCP 桥接） | `core/src/plugin.rs`、`plugins/example-hello` | `/plugins`；实测插件工具调用 |
| HTTP 服务端（SSE/会话/导出/评估/OpenAPI 3.1） | `crates/owo-agent-server` | `owo-agent serve`；`GET /openapi.json` 可生成 SDK；冒烟 + 导出 200 |
| 审计入库（SQLite audit 表） | `core/src/sqlite_store.rs` | 回合后自动追加；实测 permission/tool_call 两行落库 |
| IPC 延迟基准 | `main.rs`（`run_bench`） | `owo-agent bench --requests 200`；实测 p50 320µs / p95 650µs（目标 <5ms） |

## 二、技术文档 v1 P0 对照

| P0 项 | 状态 |
|---|---|
| Agent SDK 核心（loop/工具/上下文/会话/审计） | ✅ |
| 权限与审批（deny/ask/allow、独立审批接口） | ✅ |
| 模型网关（OpenAI-compatible/Anthropic 预留、流式、用量） | ✅（流式/工具；用量统计待补） |
| 执行环境（本地沙箱 workspace 校验） | ✅（OS 级沙箱为后续） |
| AGENTS.md + Skills + 子代理 | ✅ |
| MCP 工具生态（stdio/HTTP） | ✅ |
| 客户端形态（CLI/TUI、HTTP API） | ✅（Tauri 桌面为后续） |
| 插件 SDK（本地 manifest/权限/工具） | ✅（视图插槽/签名市场为后续） |
| 文本层桌面控制（注入/剪贴板/只读上下文） | ⚠️ 接口预留，桌面客户端阶段落地 |
| 本地优先数据（SQLite/会话/分享） | ✅ |
| 评估与可观测（evals/traces/审计） | ✅ |

## 三、质量门禁

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets   # 0 警告
cargo test --workspace                    # 全绿（core/cli/server）
scripts\run-eval-gate.ps1 -Threshold 0.8  # 评估门禁
scripts\skill-gate.ps1                    # 内置技能端到端门禁
```

## 四、真实模型实测记录

- 读/写/审批/审计闭环：DeepSeek 生成摘要并写入文件 ✅
- 流式输出（纯文本与工具调用）✅
- MCP stdio 与 HTTP 工具调用 ✅
- 子代理 explore 调查代码库 ✅；@直呼 ✅
- Skills use_skill ✅；上下文压缩事件 ✅
- 会话 fork/rewind/redo/tree/undo-msg ✅
- /share 导出与 HTTP export ✅；SQLite 跨进程恢复 ✅
- 内置 eval 5/5 通过 ✅；插件工具调用 ✅

## 五、已知限制与后续

- 用量统计（token/成本）、审计入库（FTS5/向量）、OS 级沙箱、文本注入、Tauri 桌面工作台、云执行、公开市场、多格式笔记、computer-use：属 v1 增强或 v2/M4 路线。
- 云端 /share 链接、可视化工作流、主题扩展（自定义色板）未实现。

## 六、v0.4 迭代记录（2026-08-12，P1/P2/P3 SDK 地基）

| v0.4 项 | 状态 | 证据 |
|---|---|---|
| 审计：v0.3 路线完成度 | ✅ M1/M2 完成，M3/M4 待办 | `builGoal/技术路线完成度审计-2026-08-12.md` |
| 设置组（stt/explore/proactive/skills/whitelist） | ✅ | settings.rs 默认值 + 部分配置解析测试 + `settings.example.json` |
| 应用白名单（D25） | ✅ | whitelist.rs：分级/敏感默认禁止/全屏游戏启发式；3 个契约测试 |
| 全域情景感知（D19/D22） | ✅ SDK 层 | perception.rs：L0-L3、快照、掩码、L2 环形缓冲不落盘、SSE 订阅；5 个契约测试 |
| 操作学习（D23/D26） | ✅ SDK 层 | learn.rs：录制/暂停/清空/敏感熔断、动作图、流程技能包存取删、主动建议阈值/频控/静默；6 个契约测试 |
| 内置技能包（D18） | ✅ 包结构 + 校验 | skills/{documents,spreadsheets,pdf,browser}（SKILL.md+manifest+tests/3 用例）；skill_pack.rs 校验/发现/安装测试；serve 启动自动安装 |
| v0.4 HTTP 接口 | ✅ | context.snapshot / perception.events(SSE) / learn.* / skill.verify / proactive.* / whitelist.*；OpenAPI 补充；本机冒烟通过（含 UTF-8 中文路径） |
| CLI 接入 | ✅ | `/whitelist`、`/perception`、`/learn`、`/proactive` |
| 桌面工作台 Web 壳（P1 骨架） | ✅ | `desktop/web/`：任务列表、对话 SSE 流式、审批条、diff 审阅、技能中心、感知状态区、白名单管理；`owo-agent serve` 在 `/` 静态托管；GET /、/app.js、/style.css、/sessions、/skills 冒烟通过 |
| L0 前台窗口事件源（P2） | ✅ Windows | platform.rs（Win32 GetForegroundWindow/QueryFullProcessImageNameW）；`/context/snapshot` 自动刷新并去重；冒烟实测捕获 Obsidian 前台窗口且不重复记录 focus |
| 内置技能真实执行链路（P1） | ✅ | `skills/*/tests/run_tests.py|js` 可执行契约测试：docx 生成/修改/结构校验、xlsx 生成/公式/CSV 往返、PDF 生成/AcroForm 填写/渲染校验、浏览器导航/表单/截图+DOM；`scripts/skill-gate.ps1` 全绿；可并入 `run-eval-gate.ps1 -SkillGate` |
| Tauri 2 桌面主客户端（P1） | ✅ 骨架可运行 | `desktop/tauri/src-tauri`：加载 Web 工作台、自动拉起核心服务（4096）、退出回收子进程、托盘（显示/退出）、全局快捷键 Ctrl+Alt+Shift+O（注册失败降级继续）；clippy 干净；冒烟：桌面启动后核心服务就绪、CORS 预检 200 |
| L0 剪贴板事件源（P2） | ✅ Windows | `GetClipboardSequenceNumber` 轮询 + 掩码事件（不读取内容）；冒烟：剪贴板变化后快照出现 copy_masked 且去重 |
| L2 按需截图（P2） | ✅ Windows | GDI BitBlt/GetDIBits → 内存 BMP 环形缓冲（5 帧、不落盘）；快照仅暴露元数据；4x4 采样测试 + 环形缓冲/销毁断言 |
| L1 无障碍 UI 树（P2） | ✅ Windows | accessibility.rs（UI Automation：角色/名称/类名语义锚点，深度/节点截断，变化去重）；快照 `ui_context.ui_tree` 冒烟实测 19 节点（Obsidian 前台窗口） |

### 下一迭代（P1 剩余 / P2）

- L2 本地摘要（视觉模型/OCR）、语音 STT 插件（SenseVoice-Small）。
- Tauri 打包发布（安装包/自动更新/常驻自启）与核心服务版本管理。
