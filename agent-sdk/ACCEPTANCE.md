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
| L2 本地 OCR 摘要（P2） | ✅ Windows | ocr.rs（Media.Ocr 离线识别，摘要仅进内存帧元数据）；`/perception/capture`（width/height 可采样）+ `/perception/layers` 逐层授权；冒烟：L2 关闭时 400、开启后 8x8 采集成功且快照进入 l2_visual |
| P3 动作图执行引擎 | ✅ 核心 | executor.rs：UiActionSource 抽象 + Windows 实现（UIA 锚点定位、InvokePattern/可点击点、SendInput Unicode/快捷键、前台标题验证）；图遍历（变量填充/验证/成环检测/步数上限/敏感面熔断）；`POST /learn/execute`；5 个契约测试 + 冒烟（敏感面 blocked、锚点缺失 failed 且不注入输入） |
| P3 示范学习流水线 | ✅ SDK 层 | learn.rs：录制→泛化（重复 Type 锚点推断 `{value}`）→沉淀流程技能包；`/learn/execute` 分步审计入库；2 个契约测试 |
| P3 桌面闭环 UI | ✅ | Web 工作台：录制控制/沉淀表单/流程技能包列表一键执行/主动建议四选；`/learn/start|stop|packages|sink|execute-package`、`/proactive/suggestions`；冒烟：录制 2 样本 → stop → 沉淀 send-file → 列包成功 |
| P3 执行审批 + 自动观察 | ✅ 审批已验 / 观察已接线 | `/learn/execute*` 无 `confirm:true` 返回 400（冒烟验证）；确认后执行并写 approval 审计；`start_observer` 录制中 2s 采样前台/剪贴板（掩码、去重）——当前会话无前台/剪贴板可用，运行时采样待桌面会话验证 |
| P3 高敏感二次确认 | ✅ | `sensitivity=high` 执行需 `high_risk_ack:true`（冒烟：无 ack 400，有 ack 安全失败不注入）；确认写审计；Web 端二次确认对话框 |
| 流程技能包分享（D26） | ✅ | share_skill.rs：`.owskill` ZIP 导出/导入（4 个契约测试：往返、未知权限拒绝、敏感度必填、zip-slip）；`/learn/export/{name}` + `/learn/import` 冒烟：导出 830B → 导入回写成功；Web 端导出/导入按钮 |
| 语音输入兜底 + 桌面自启 | ✅ | Web 工作台 🎤（系统语音识别转写进输入框）；Tauri 托盘“开机自启”切换 HKCU Run（winreg），编译通过 |
| 本地 STT（D20） | ✅ 引擎 + 实机推理 | stt.rs：sherpa-onnx + SenseVoice-Small 离线转写，`POST /stt/transcribe`；`download-stt-model.ps1`（已修正资源 URL）实测下载 239MB int8 模型；真实推理冒烟：440Hz 测试 WAV → `{"ok":true,"text":"I.","elapsed_ms":2593}`（模型加载+推理全链路）；83 测试全绿（链接期 LNK4098 为 sherpa 静态库 /MT 与 Rust /MD 的已知告警，不影响运行） |
| STT 普通话 CER 基线 + 缓存 | ✅ | 系统 TTS 生成普通话样本（文本即标准答案）→ 本地转写整句正确，**CER 0.00%（0/19 字符）**；识别器缓存后重复推理 **3.33s → 0.93s**（5s 音频 p95 <2s 预算口径达标）；注：TTS 合成语音，自然语音 WER 基线待真实语料 |
| 语音输入本地闭环（D20） | ✅ UI 已接 | 🎤 麦克风（WebAudio）→ 16k WAV 编码 → `/stt/transcribe` 本地推理 → 输入框；模型缺失/无麦克风自动回退 Web Speech；10s 自动停止；自然语音 WER 基线仍待真实标注语料 |
| Web 工作台 JS 修复 | ✅ | `node --check` 发现 `package` 为严格模式保留字导致 app.js 解析失败（自技能包列表功能起整个工作台 JS 失效），已全部改名 `pkg`；node 语法校验通过 |
| 主技术文档升级 v0.4 | ✅ | 按 v0.4 续写计划第 9 节合并：头部版本/范围、D17–D26 决策、3.1 桌面 P0、4.3 常驻进程模型、5.8 全域情景感知、6.5 操作学习与新增接口、7.6 感知隐私边界、9 路线图修订（M3–M6）、附录 B 术语；续写计划状态更新为“已合并” |
| 自动更新（updater） | ✅ 骨架+签名管线 | tauri-plugin-updater 接入：托盘“检查更新”、端点为占位 URL、真实签名公钥（私钥在 .secrets，gitignore）；`generate-update-manifest.ps1` 实测签名安装包并产出 latest.json（signature 416 字符）；编译/clippy 通过 |
| 录制自动观察实机验证 | ✅ | 修复：observer 原本被错误 spawn 进 run_bench，已移到 run_serve；实测：开始录制后 5s 自动采到 2 条掩码样本（前台去重生效），停止后待沉淀 |
| 便携打包发布 | ✅ | `scripts/package-desktop.ps1`：release 构建 → `dist/OwO-Agent-release.zip`（核心服务 + 桌面壳 + skills + README，6.8MB）；桌面壳 exe 同级定位核心服务与技能包；便携包冒烟：核心服务就绪、4 个内置技能从随包目录加载 |
| NSIS 安装程序 | ✅ | `scripts/build-installer.ps1`：externalBin 内置核心服务（`owo-agent-x64.exe` 运行时同级定位）→ `OwO Agent_0.1.0_x64-setup.exe`（4.8MB，含核心服务；简体中文/English、当前用户安装）；实际构建通过 |

### 下一迭代（P1 剩余 / P2）

- 语音 STT 插件（SenseVoice-Small）。
- Tauri 安装包（NSIS/MSI）/自动更新/常驻自启与核心服务版本管理（便携 zip 已可用）。
- SenseVoice-Small 自然语音 WER 基线（需真实普通话语料；合成语音 CER 0.00% 已记录）。
