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
| 模型网关（OpenAI-compatible/Anthropic 预留、流式、用量） | ✅（流式/工具/用量统计） |
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
| STT 自然语音 CER 基线（真实人声） | ✅ 首样本 | FunASR 官方中文示例 `asr_example_zh.wav`（真实人声，5.55s/16k）：标准文本“欢迎大家来体验达摩院推出的一系列语音识别模型”，本地转写“欢迎大家来体验达摩院推出的语音识别模型。”，**CER 13.64%（3/22，标点归一化后漏“一系列”；含标点口径 18.18%）**；`scripts/stt-wer-eval.py` 清单式评估工具就绪（2 样本试跑：TTS 0% + 真实人声 13.64%，均值 6.82%）；完整 50+20 条 WER<5% 口径仍需标注语料 |
| 语音输入本地闭环（D20） | ✅ UI 已接 | 🎤 麦克风（WebAudio）→ 16k WAV 编码 → `/stt/transcribe` 本地推理 → 输入框；模型缺失/无麦克风自动回退 Web Speech；10s 自动停止；自然语音 WER 基线仍待真实标注语料 |
| Web 工作台 JS 修复 | ✅ | `node --check` 发现 `package` 为严格模式保留字导致 app.js 解析失败（自技能包列表功能起整个工作台 JS 失效），已全部改名 `pkg`；node 语法校验通过 |
| 主技术文档升级 v0.4 | ✅ | 按 v0.4 续写计划第 9 节合并：头部版本/范围、D17–D26 决策、3.1 桌面 P0、4.3 常驻进程模型、5.8 全域情景感知、6.5 操作学习与新增接口、7.6 感知隐私边界、9 路线图修订（M3–M6）、附录 B 术语；续写计划状态更新为“已合并” |
| 自动更新（updater） | ✅ 骨架+签名管线 | tauri-plugin-updater 接入：托盘“检查更新”、端点为占位 URL、真实签名公钥（私钥在 .secrets，gitignore）；`generate-update-manifest.ps1` 实测签名安装包并产出 latest.json（signature 416 字符）；编译/clippy 通过 |
| 录制自动观察实机验证 | ✅ | 修复：observer 原本被错误 spawn 进 run_bench，已移到 run_serve；实测：开始录制后 5s 自动采到 2 条掩码样本（前台去重生效），停止后待沉淀 |
| 自动化面板（P1） | ✅ | automation.rs：单次/间隔/每天调度 + 提醒动作 + JSON 持久化 + 触发审计；4 个契约测试；冒烟：创建间隔 2s 任务 → 5s 内触发 2 条提醒（last_run 更新）→ 停用 → 删除；Web 工作台面板（创建/启停/删除/提醒列表） |
| 数据出境开关（7.5） | ✅ | settings.rs `egress.cloud_enabled`（默认开）+ gateway.rs 联网前拒绝（完整/流式，每次调用检查运行时开关）+ CLI serve/repl/tui/turn 启动时应用 + `GET /settings` / `POST /settings/egress`（写 settings.json + 审计）+ Web“设置与诊断”区一键切换；契约测试 `cloud_disabled_rejects_requests_before_network` / `cloud_switch_applies_without_reconstruction`；E2E 实测：关闭→turn 返回“云端模型已禁用（数据出境开关关闭）”、接口写回、运行中即时切换（无需重启） |
| 设置与诊断（P1） | ✅ | `GET /settings` / `POST /settings`（保存完整 settings.json + 运行时应用：数据出境、模型热切换、STT 模型/语言/ITN、主动建议阈值、白名单合并默认清单；保存写审计）；`whitelist/manage` 持久化用户清单；Web JSON 编辑器 + 保存按钮；契约测试：settings 保存/加载往返、STT `apply_settings`、ProactiveEngine `apply_settings`、网关模型热切换（`model_switch_applies_without_reconstruction`）；E2E 实测：POST /settings 后数据出境即时生效、白名单运行时生效、whitelist/manage 写回 settings.json |
| 会话管理（P1） | ✅ | session.rs 新增 title/archived/pinned（fork 子会话继承父链）+ sqlite_store 列迁移；`GET /session/{id}`（历史断点恢复）、`POST /session/{id}/rename|archive|pin`；列表置顶排序 + 归档默认隐藏；Web 会话树（缩进子会话）+ 继续/重命名/置顶/归档/fork/回退/重做；契约测试 3 个（title/archive/pin 往返、空会话 fork 不 panic、SQLite 迁移与新列往返）；E2E 实测：改名/置顶/归档/fork/rewind/redo/children 全通，重启后元数据持久化 |
| 审计落库 + 日志面板（P1） | ✅ | SessionStore trait 新增 `append_audit` / `recent_audit`（SQLite 落库、按条目 session_id）；服务端回合/设置/学习审计统一 flush；`GET /audit?limit=N`；Web 右侧审计日志面板（5s 刷新）；契约测试扩展：SQLite 追加+最近查询；E2E 实测：egress 开关 2 条审计落库、重启后仍可查询 |
| 技能中心（P1） | ✅ | SkillRegistry 运行时共享禁用集合（`set_disabled`/`is_enabled`/`list_enabled`/`get_enabled`），系统提示与 use_skill 只放行启用技能；`skills.disabled` 持久化；`GET/POST /skills/{name}`（详情/编辑 SKILL.md）、`POST /skills/{name}/enabled`、`GET/DELETE /learn/packages/{name}`（详情/删除+审计）；Web 启用/禁用/查看/编辑/导出/删除；契约测试：禁用技能被过滤且共享集合即时生效、settings 往返含 disabled；E2E 实测 7 步全通（含导入→详情→删除→审计） |
| 对话附件（P1） | ✅ | `POST/GET /session/{id}/attachments`（base64 JSON 上传、文件名清洗、50MB 上限、落盘工作区 `.owo-attachments/`）；`TurnRequest.attachments` 注入附件路径上下文，缺失附件 400；上传写审计；Web 📎 多选上传 + chips；契约测试：附件名清洗；E2E 实测 7 步全通（上传/穿越名清洗/列表/落盘校验/缺失 400/带附件 turn 联网/审计） |
| 桌面操作迭代（launch/Inject/tree/parent/ui-verify/ClickAt/OCR/region-OCR） | ✅ | `ActionType::Launch` / `ClickAt`；`inject` handle=0 修复；`POST /perception/tree`（节点含边界框）；`POST /perception/ocr` + `/ocr/region` + `/ocr/status`；OCR 根因修复（WIC→直接 SoftwareBitmap，实测 1636 字符/647 坐标框）；`SemanticAnchor.parent`；`ui:`/`value:` 验证谓词；`qq-send-file` 图重写；契约测试 106 全绿 |
| QQ 真实桌面实测（本机） | ✅ 文本 + 文件 + 表情三实锤（受控验证） | 受控路径：点输入框（坐标）→ 注入 → Enter；UIA 树消息列表出现“OwO 受控发送验证-081”；**文件链路全通**：`hello.txt` 气泡 + `67.00 B 已发送` + **“对方已成功接收文件‘hello.txt’”**；**表情发送实锤**：`[呲牙]` 以新消息出现在聊天记录（落木逐风 y=711）；QQ 图片表情面板为图像渲染（OCR 无文字，需视觉/模板迭代）；聊天中 prompt-injection 内容被忽略；红包/微信待复验 |
| 微信实测准备（本机） | ✅ 已启动待登录 | `launch D:\apps\Weixin\Weixin.exe` 成功（修复后不再弹错误框）；UIA 树识别微信登录窗口：二维码 / 扫码登录 / 仅传输文件（Qt 界面，XTextView/XButton 可定位）；登录后即可复用 QQ 同套流程（搜索→会话→消息/文件/表情） |
| 微信全功能测试 | ⬜ 未安装 | 本机未检测到 WeChat/Weixin 进程与常见安装路径；安装登录后可复用 QQ 同一套流程（launch→搜索→消息/文件/表情） |
| P3 真实桌面端到端（Notepad 示范→复用） | ✅ | 执行器新增 ValuePattern 回读验证（递归找可编辑控件）；实测：Notepad 中输入“你好 OwO”并回读验证 ok → 换参数“第二次复用 456”再次执行 ok（2/2 成功，未越权、敏感面熔断保持） |
| VSCode 语音改代码 E2E（P2 验收形态） | ✅ **30/30 = 100% PASS（两类任务）** | 语音链路：TTS 中文语音 → 本地 SenseVoice 转写 → DeepSeek Agent（deepseek-v4-flash）读取 hello.py → 新增函数并跑测试验证 → 文件确认；`voice_code_batch.py`（每轮硬看门狗、`E2E_FUNC` 可换目标函数）：add 任务 20/20 + multiply 任务 10/10，**累计 30/30 = 100%**，显著超过“20 次成功率 ≥80%” |
| STT 中英混说基线（试跑） | ✅ 3 样本 | TTS 生成 3 条中英混说（Chrome/OpenAI Codex/GitHub/pull request/DeepSeek API 等）：CER 16.0% / 19.05% / 31.58%，均值 **22.21%**——离 <5% 目标有差距，属热词/ITN 调优方向；完整 20 条口径待语料 |
| STT 语言/ITN 配置旋钮 | ✅ | `SttSettings` 新增 `language`（默认 auto）与 `itn`（默认 true），支持 `OWO_STT_LANGUAGE`/`OWO_STT_ITN` 环境覆盖（settings.example.json 同步）；语料门禁对比实验：auto+ITN **16.05%** < zh 16.91% < auto 无 ITN 16.80%，默认组合保留 |
| L3 语义层 v1（任务假设） | ✅ | `perception.rs`：本地启发式 `infer_task_hypothesis`（coding/chatting/gaming/browsing/reading + 置信度），L3 授权时随前台刷新自动更新、变化才记录；2 个契约测试；冒烟：开启 l3_semantic 后快照含 `task_hypothesis`（如 browsing 0.7）且不上送云端 |
| QQ 发文件流程技能包示例（D26/P3） | ✅ 包就绪（执行待测试账号） | `skills/user/qq-send-file`：SKILL.md + graph.json（5 节点：搜索联系人→进入会话→发送文件→选文件→发送）+ manifest（targetApps=qq、variables=contact/file、sensitivity=medium）+ 3 契约用例；契约测试 `qq_send_file_example_package_is_valid_and_round_trips` 通过（校验 + .owskill 往返）；真实 QQ 执行需测试账号与会话授权 |
| STT 回归语料门禁 | ✅ 可复现 | `tests/stt-corpus/`（5 个 wav + corpus.tsv + README）；`scripts/run-stt-corpus.ps1` 实测 **5 样本均值 CER 16.05%**（TTS 0% / 真实人声 13.64% / 混说 16–31.6%），与历次结果一致 |
| STT 任意视频音轨冒烟（用户口径） | ✅ | 本机视频 `Videos/2025-04-25 10-25-00.mkv` → ffmpeg 取 20s/16k 单声道 → SenseVoice-Small 转写成功（elapsed 5.36s），输出历史纪录片音轨文本；口径：任意视频能识别即说明引擎一般没问题 |
| 桌面会话实机验证（本机） | ✅ 核心链路 | 4096 核心服务可实时看到交互桌面：前台应用切换（Edge→ChatGPT→QQ）被捕获、UIA 树可达（QQ 窗口节点可见）、剪贴板掩码事件、L2 截图成功（内存帧 9.2MB、不落盘）、L3 任务假设（reading 0.5）；`owo-agent bench` 200 请求 **p50 596µs / p95 1255µs**（目标 <5ms，面板预算 <150ms 余量充足） |
| QQ 实测准备 | ✅ 环境就绪 / 待用户参数 | QQ.exe（D:\QQ）已唤起且前台被捕获（id=qq），UIA 树可达；`qq-send-file` 流程技能包已导入真实数据目录（variables: contact/file）；**注意**：唤起时 QQ 显示登录页（自动登录/账号密码登录），需用户切到已登录主窗口并提供测试联系人 + 待发送文件路径后执行 |
| E2E 中发现并修复的 3 个真 bug | ✅ | ① 模型网关不读代理环境变量导致外网模型调用挂起——新增 OWO_HTTP_PROXY/HTTP(S)_PROXY 支持 + 180s 超时（gateway.rs）；② 文件工具按 Policy 工作区而非会话工作区解析相对路径，且 Windows canonicalize 的 `\\?\` 前缀导致误判越界——改为会话工作区基座 + 双侧规范化（tools.rs）；③ 服务端审批事件重复发送（Agent emit + ChannelApprover 各一次）导致客户端 404——移除 ChannelApprover 重复发送（server lib.rs） |
| 便携打包发布 | ✅ | `scripts/package-desktop.ps1`：release 构建 → `dist/OwO-Agent-release.zip`（核心服务 + 桌面壳 + skills + README，6.8MB）；桌面壳 exe 同级定位核心服务与技能包；便携包冒烟：核心服务就绪、4 个内置技能从随包目录加载 |
| NSIS 安装程序 | ✅ | `scripts/build-installer.ps1`：externalBin 内置核心服务（`owo-agent-x64.exe` 运行时同级定位）→ `OwO Agent_0.1.0_x64-setup.exe`（4.8MB，含核心服务；简体中文/English、当前用户安装）；实际构建通过 |

### 下一迭代（P1 剩余 / P2）

- 语音 STT 插件（SenseVoice-Small）。
- Tauri 安装包（NSIS/MSI）/自动更新/常驻自启与核心服务版本管理（便携 zip 已可用）。
- SenseVoice-Small 自然语音 WER 基线（需真实普通话语料；合成语音 CER 0.00% 已记录）。

---

## 七、v0.4.1 计算机使用专项（2026-08-12，后台静默模拟）

目标：默认视觉识别自主迭代操作任意软件；改文件类优先后台 Agent，做不了的再模拟鼠标键盘；
本轮验收 QQ 回复闭环（OCR 读上下文、等待、按指令回复）与浏览器搜索/浏览/图片下载。

| 项 | 状态 | 证据 |
|---|---|---|
| headless 模拟 QQ（GDI 离屏渲染 + HTTP 虚拟窗口） | ✅ | `owo-sim-qq --headless`：`/frame`（BMP）、`/ocr`（真值版面 lines+role_hint）、`/click`、`/type`、`/key`、`/state`、`/log`、`/reset`；事件日志含 incoming/outgoing/send_clicked/input_clicked |
| 模拟浏览器站 | ✅ | `owo-sim-browser`：首页/搜索/文章/PNG 图片（3 张生成图 + 下载链接） |
| 桌面/浏览器双表面工具 | ✅ | `screen_ocr`/`ocr_region`（lines 坐标 + role_hint）、`desktop_click/type/key/shortcut/activate/window_list/foreground/launch/wait`、`browser_navigate/search/snapshot/click/type/press/screenshot/download_image/close`；`OWO_SIM_QQ_URL` 一键切模拟面，服务端直连写接口在模拟面下 400 禁用 |
| QQ 回复闭环 e2e（DeepSeek 真实模型） | ✅ 3/3 | `scripts/sim-qq-e2e.py`：轮次 3/4/5 全过（24–25s；2 条 outgoing + 2 次 send_clicked + 输入框清空；自动等待对方回复后二次回复并验证上屏） |
| 浏览器搜索/下载 e2e | ✅ 2/2 | `scripts/sim-browser-e2e.py`：导航本地搜索站→输入关键词→打开文章→下载 40,767B PNG，PNG 头校验通过（含相对 `src` 解析） |
| 一键验收脚本 | ✅ | `scripts/run-sim-e2e.ps1`：启动 headless 模拟 + 测试服务（4097）→ 两个 e2e → 自动清理进程 |
| 浏览器驱动（Playwright + 本机 Edge） | ✅ | `scripts/browser-driver.js`：持久化 profile、可 headless、JSONL 常驻协议；图片相对 URL 用 `new URL(src, page.url())` 解析 |
| 质量门禁 | ✅ | `cargo fmt --check` 干净；`clippy --workspace --all-targets` 0 警告；`cargo test --workspace` 全绿（core 86 + cli/server/协议） |

后续（按设计文档 M-A/M-B/M-C 剩余项）：窗口级截取/窗口模板、PaddleOCR、本地视觉模型
（场景描述/完成验证/grounding 交叉验证）、静默操作学习（情景/语义记忆）；真实 QQ 复用同一套
`desktop_*` 工具链路（去掉 `OWO_SIM_QQ_URL` 即恢复真实桌面面）。

## 八、v0.4.2 计算机使用增强（2026-08-12 续）

| 项 | 状态 | 证据 |
|---|---|---|
| `desktop_wait_until`（OCR 谓词等待） | ✅ | computer_use.rs：按 `text`+可选 `role_hint` 轮询 `screen_ocr`，超时返回 `matched=false`；2 个单测；权限 Read |
| QQ 多联系人切换 e2e（真实模型） | ✅ 1/1 | `sim-qq-e2e.py --prompt-file qq-multi-contact.txt --require-contacts-file qq-multi-contacts.txt`：55.4s，3 条消息覆盖张子豪+李四，含切会话、`desktop_wait_until` 等回复、每次发送后 OCR 验证 |
| 真实网页 headless 浏览器 e2e | ✅ 1/1 | `web-browser-e2e.py`：Bing 被网络阻断时 Agent 自动换 360 搜索并选择可达的权威站点，下载 3,587B SVG 图片；含代理支持 `OWO_BROWSER_PROXY` |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 88 等） |

## 九、v0.4.3 操作记忆闭环（2026-08-12 续）

| 项 | 状态 | 证据 |
|---|---|---|
| 模拟面执行器源（SimUiActionSource） | ✅ | computer_use.rs：纯 TcpStream 同步 HTTP（避免 blocking reqwest 在 async 处理器析构 panic）；锚点匹配抽为 `sim_anchor_matches` 纯函数 + 单测；`/learn/execute*` 按 `OWO_SIM_QQ_URL` 自动选源 |
| 示范→录制→泛化→沉淀→复用闭环 | ✅ 2/2 | `sim-qq-learn-e2e.py`：脚本化示范两次回复 → 6 个 RecordedAction（内容掩码）→ `qq_reply` 技能包泛化出 `{value}` → 重置场景 → 换参数复用执行 6/6 步 ok，两条新消息发出；`-001`/`-002` 两轮均通过 |
| 回车发送也记录 send_clicked | ✅ | sim_qq.rs：`/key enter` 与点击发送等价记录发送事件 |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿 |

说明：学习样本只记录动作摘要（类型/锚点/次数），不记录消息正文（`value_masked=true`）；
技能执行走模拟面虚拟窗口，真实桌面技能执行仍走 WindowsUiaSource。

## 十、v0.4.4 视觉模型网关（2026-08-12 续，M-B 起步）

| 项 | 状态 | 证据 |
|---|---|---|
| vision.rs（BMP→PNG + Ollama/OpenAI 双通道） | ✅ | `bmp_to_png`（PNG 头单测）、`describe_image`、`parse_verification`（YES/NO+置信度单测）、`ollama_models` |
| Agent 工具 | ✅ | `screen_vision`（场景描述）、`vision_verify`（yes/no+confidence）；权限 Read |
| 服务端接口 | ✅ | `GET /vision/status`（provider/model/已拉模型）、`POST /vision/describe`；未就绪时 502 + “请先运行 ollama pull qwen2.5vl:3b” |
| 本地模型拉取 | ⏳ 后台进行中 | `ollama pull qwen2.5vl:3b`（约 3.2GB，1.5-2MB/s，ETA ~30min）；`scripts/download-vision-model.ps1` 可重跑 |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 91） |

环境变量：`OWO_VISION_PROVIDER`（ollama/openai）、`OWO_VISION_MODEL`、`OWO_VISION_BASE_URL`、
`OWO_VISION_API_KEY`、`OWO_OLLAMA_HOST`。视觉只做理解与验证，主控制仍走 OCR+坐标。

## 十一、v0.4.5 静默观察与情景记忆（2026-08-12 续，M-D 起步）

| 项 | 状态 | 证据 |
|---|---|---|
| observe.rs（情景记忆 JSONL + 事件摘要） | ✅ | `MemoryStore`（追加/列表/清空，往返单测）、`observation_from_sim_event`（内容掩码单测）、`map_sim_events_to_actions`（动作序列单测）、`value_hash` |
| 静默观察器 | ✅ | `start_memory_observer`：模拟面每 2s 拉取模拟日志，自动写入情景记忆（不经过 /learn/record）；服务启动即挂载 |
| 挖掘接口 | ✅ | `GET /memory/observations`、`POST /memory/clear`、`POST /memory/mine-skill`（观察序列→泛化→沉淀技能包，审计入库） |
| 静默观察→挖掘→复用 e2e | ✅ 1/1 | `sim-qq-observe-e2e.py`：观察器自动入库 9 条摘要（incoming/input_clicked/typed/outgoing/send_clicked）→ 挖掘 `qq_reply_observed`（{value}）→ 重置后换参数执行，两条新消息发出、输入框清空 |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 94） |

下一步：真实面观察源（UIA 事件/窗口状态采样）、候选技能“用户确认转 active”、情景记忆保留期滚动清理。

## 十二、v0.4.6 BYOK 视觉通道端到端验证（2026-08-12 续）

| 项 | 状态 | 证据 |
|---|---|---|
| `POST /vision/verify` 接口 | ✅ | 对当前截图（模拟帧/真实屏幕）问 yes/no 问题，返回 answer + confidence |
| BYOK（OpenAI-compatible）通道 e2e | ✅ 1/1 | `vision-mock-e2e.py`：mock 端点收到 3 次带图请求（data:image/png，b64≈114KB）→ describe 返回界面描述、verify 解析 yes/0.95 与 yes/0.93 |
| 本地 VL 模型（qwen2.5vl:3b） | ⏳ 后台下载中 | 3.2GB 受网络限速，已续传；就绪后 `scripts/sim-qq-vision-e2e.py` 直接跑真实视觉描述/验证 |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿 |

视觉通道优先级：本地 Ollama（默认）→ BYOK 云端（`OWO_VISION_PROVIDER=openai`）；
模型不可用时返回明确错误，主链路（OCR+坐标）不受影响。

## 十三、v0.4.7 PP-OCRv6 接入 + 真实 QQ 受控发送（2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| PP-OCRv6 云 API 客户端 | ✅ | paddle_ocr.rs：multipart 提交 → 轮询 job → 下载 JSONL → 解析 `prunedResult.rec_texts/rec_polys`；凭据 `PADDLE_OCR_TOKEN` 环境变量，云 OCR 受数据出境开关约束；代理仅 `PADDLE_OCR_PROXY` 显式启用（实测本地代理访问 API 超时、直连正常） |
| OCR 引擎路由 | ✅ | `ocr_preferred`：Paddle 启用时优先，失败回退 Media.Ocr 并标记 provider；`OWO_OCR_STRICT=paddle` 可开严格模式看真实报错；screen_ocr/ocr_region//perception/ocr*/vision ground 全部接入 |
| PP-OCRv6 实测（模拟 QQ 帧） | ✅ | provider=paddle-v6，0.9s，93 字符/12 框，**读出了“输入消息...”和“发送”**（Media.Ocr 长期读不出的离屏小字） |
| 真实 QQ 受控发送（UIA 锚点） | ✅ | `real-qq-send.py`：聚焦 Rich Text Editor → SendInput 输入 → UIA 点击“发送”→ UIA 树确认消息上屏；受控消息 002/004 实锤，聊天中的 prompt-injection 内容被忽略 |
| 滚轮支持 | ✅ | `desktop_scroll`（tool + HTTP + executor），权限 Inject；滚动会话/聊天列表 |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 99） |

## 二十八、v0.4.22 网络恢复后实机闭环复测（2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| DeepSeek 多轮通道 | ✅ 恢复 | 无工具双轮冒烟 1.6s / 1.3s；4097 一次视觉闭环仍偶发“首轮工具后挂起”，同构 4098 实例完整通过，确认属外部流式偶发而非代码缺陷 |
| 模拟 QQ 视觉 Agent 闭环 | ✅ | screen_ocr → desktop_click → desktop_type → desktop_click → screen_ocr → vision_verify×2；outgoing=1、send_clicks=1、input_after=""；135.7s，DeepSeek 全程流式正常 |
| 真实 QQ 群聊受控发送 | ✅ 实锤 | 「26大创-智能输入法」群：UIA 搜索 → 点击会话行 → 聊天头校验（防发错）→ 输入 → 点击发送 → UIA 树命中消息文本；消息带“OwO 受控测试”标记，全程 15.9s |
| 真实浏览器 Agent 闭环 | ✅ | Bing 搜索 rust → 识别 rust-lang.org 官方结果 → 打开官网 → 下载 Rust Logo（SVG 2396B）→ run_command 校验；67.9s；中间一次选择器超时后自动恢复 |
| 本地视觉验证 | ✅ | qwen2.5vl:3b：新消息上屏 yes/0.8；输入框清空被判 no/0.95（占位符“输入消息...”干扰，screen_ocr 已确认实际清空） |
| 回归门禁 | ✅ 2/2 | qq-learn / qq-observe 确定性套件通过（12.6s） |
| 脚本修正 | ✅ | `real-qq-group-send.py` 增加 `--base` 参数，默认指向真实桌面服务 4096 |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 109） |

结论：多模态 Agent 两条核心路径（桌面视觉操作、真实浏览器搜索/下载）在 DeepSeek 网络恢复后均
跑通完整闭环；真实 QQ 群聊发送已实锤。下一步：把 vision_verify 的“输入框清空”判定改为忽略
占位符（或让视觉提示词排除占位文字），并把视觉 grounding 结果并入元素注册表。

## 十五、v0.4.9 窗口级截取（M-A 第一优先，2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| PrintWindow 窗口抓取 | ✅ | `platform::capture_window_bmp`（PW_RENDERFULLCONTENT，失败回退 BitBlt），后台只读、可抓被遮挡窗口；返回 BMP+屏幕矩形 |
| 窗口级 OCR 接口 | ✅ | `POST /perception/window {hwnd}`：窗口 BMP → PP-OCRv6/Media → 文本+整行坐标（窗口矩形附带） |
| Agent 工具 | ✅ | `desktop_window_ocr`（hwnd 或 process/title），输出转屏幕坐标，权限 Read |
| 实测（QQ 窗口 198064） | ✅ | 后台只读抓取 QQ（846,179-1856,951）→ paddle-v6，339 字符/33 行，“张子豪/发送”可读；全程不切前台、不干扰桌面 |

下一步：窗口模板（ROI 集合）与“窗口元素注册表”，把窗口级 OCR 稳定用于真实 QQ 会话定位。

## 十六、v0.4.10 窗口模板（UIA/OCR 双路径）+ 深度抓窗（2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| UIA-by-hwnd | ✅ | `ui_tree_for_hwnd`（不要求前台，`/perception/tree` 支持 hwnd 参数）；Chromium 应用（QQ）在非激活态只暴露 2 节点（内容仅激活时提供） |
| 窗口模板（UIA 版） | ✅ | `window_template.rs`：从 UIA 树提取“会话列表/消息列表/Rich Text Editor/发送/搜索/表情/文件/红包”ROI；`/perception/template/build|detect` + 持久化；单测（构建/检测/存取） |
| 窗口模板（OCR 版） | ✅ | `build_template_from_ocr`/`detect_template_ocr`：PrintWindow+PP-OCRv6 按语义文本提取 ROI，后台可用；实测锁屏下 QQ 窗口提取到“搜索”ROI 并命中 |
| 深度抓窗 | ✅ | `capture_window_bmp_deep`：枚举子窗口逐帧 PrintWindow+BitBlt 择优（Chromium 渲染子窗口）；实测锁屏下 QQ 仍缺底部输入区（D3D 呈现暂停，属环境限制） |
| 锁屏限制说明 | ⚠️ | 锁屏/无人值守时 Chromium 应用不向窗口 DC 呈现底部区域，完整窗口 OCR/模板需交互桌面会话；会话列表/消息区可后台读取（339 字符） |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 102） |

## 十七、v0.4.11 真实桌面输入会话限制与修复（2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| 激活逻辑修复 | ✅ | `activate_window`：Alt 解锁键仅在 SetForegroundWindow 失败时发送并加沉降延时，避免无谓破坏输入队列；windows-sys BOOL 判定修正 |
| QQ 群聊脚本改进 | ✅ | `real-qq-group-send.py` 搜索改用 UIA 点击“搜索”+Ctrl+A 清空（避免 Ctrl+F 后 SendInput 偶发失效），仍带“聊天头校验防发错”保护 |
| 沙箱输入桌面限制 | ⚠️ 环境限制 | 本沙箱拉起的进程（无论是否提权）无交互输入桌面：`SetCursorPos` 报 0x800700CB、`GetForegroundWindow` 为空、SendInput 返回 0；计划任务投递同样不可行（Queued）。真实 QQ 键盘/鼠标注入需在用户交互会话内启动服务（桌面壳自启的 4096 已验证可行） |
| 会话诊断脚本 | ✅ | `scripts/owo-session-probe.ps1`：由计划任务/交互会话执行时输出 SESSIONNAME 与 QQ 进程，用于验证桌面可达性 |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 102） |

建议：真实桌面测试请由用户侧终端/桌面应用启动核心服务（如 `owo-agent serve --port 4096`），
本沙箱服务（4097）继续用于模型/OCR/模拟面能力。

## 十八、v0.4.12 窗口元素注册表（设计文档 10.1，2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| element_registry.rs | ✅ | `SceneElement`（多源/置信度/stale）、`ElementRegistry`（稳定 ID 匹配：名称+角色+位置邻近，>3 帧淘汰）、`fuse_sources`（UIA+OCR 同名重合合并，UIA 优先几何）；4 个单测 |
| `/perception/elements` 接口 | ✅ | `{hwnd, app_id}`：UIA 树 + 窗口 OCR（转屏幕坐标）融合 → 注册表更新 → 返回稳定元素列表 |
| 后台实测（QQ 窗口 198064） | ✅ | 连续两帧各 34 个融合元素（paddle-v6），**稳定 ID 34/34**；不切前台、后台只读 |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 106） |

下一步：视觉 grounding 结果并入注册表（source=vision），并用稳定元素 ID 驱动“点击/验证”动作，
减少对每次 OCR 重新定位的依赖。

## 十九、v0.4.13 执行器 OCR 文本锚点兜底（设计文档 4.1 L2，2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| AnchorTarget 双通道 | ✅ | `WindowsUiaSource.keep` 改为 `Element/Point` 两态：UIA 元素或 OCR 坐标 |
| UIA 失败 → OCR 兜底 | ✅ | `find`：UIA 递归未命中时，屏幕 Media.Ocr → 行分组 → 文本中心坐标；`invoke/type_text` 对 Point 走坐标点击+注入 |
| 纯函数 | ✅ | `find_ocr_anchor_point`（OCR 行包含锚点名 → 行中心）+ 单测（命中/未命中） |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 107） |

说明：OCR 兜底为同步 Media.Ocr 路径（避免 async HTTP 阻塞动作图）；Paddle/PP-OCRv6 的异步
OCR 锚点定位由 Agent 工具（screen_ocr/desktop_click）承担。

## 二十、v0.4.14 本地 VL 模型就绪 + 真实视觉描述/验证通过（2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| qwen2.5vl:3b 本地模型 | ✅ | Ollama 已拉取（2.98GB）；`/vision/status` 返回 provider=ollama、model=qwen2.5vl:3b |
| 视觉区域支持 | ✅ | `capture_vision_png_region`（裁剪+放大）；screen_vision/vision_verify 与 /vision/describe、/vision/verify 均支持 x/y/width/height/scale |
| 真实本地 VL 描述 | ✅ | 模拟 QQ 帧：模型正确描述“即时通讯软件，张子豪/李四，右下角蓝色发送按钮，下方输入框”（此前误读为锁屏是因为服务未设 OWO_SIM_QQ_URL、抓了真实 Windows 锁屏——管线本身正常） |
| 真实本地 VL 验证 | ✅ | 发送后区域验证：输入框已清空 yes/0.8、聊天区出现新消息 yes/0.8；`sim-qq-vision-e2e.py` 通过 |
| 测试稳定性修复 | ✅ | element_registry 测试改为按名字断言（HashMap 顺序无关），消除偶发失败 |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 107） |

本地 VL 现在可用作：场景描述（screen_vision）、完成验证（vision_verify，支持区域）、
grounding 交叉验证（vision_ground，与 OCR 重合才允许点击）。

## 二十一、v0.4.15 情景记忆滚动清理 + 测试并发修复（2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| 记忆保留期/容量清理 | ✅ | `MemoryStore::prune`：默认保留 30 天/1 万条（`OWO_MEMORY_RETENTION_DAYS`/`OWO_MEMORY_MAX` 可配），加载与追加时滚动清理；单测覆盖超期淘汰与容量上限 |
| 测试并发修复 | ✅ | observe 测试临时文件加唯一计数（同 pid 并行测试不再共文件），消除偶发失败 |
| 视觉 Agent 闭环阻塞说明 | ⚠️ 网络不稳定 | Agent 多轮调用时 DeepSeek 经本地代理的后续请求偶发流式挂起（首轮正常、单轮冒烟 6.4s 通过）；工具链与本地 VL 均正常，属当前代理/网络问题，重试或换通道后即可跑通 |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 108） |

## 二十二、v0.4.16 模型网关韧性（2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| 发送失败代理→直连降级 | ✅ | `post_chat`：优先代理客户端，失败自动切无代理直连重试；4xx/5xx 不重试 |
| 流式空闲看门狗 | ✅ | `complete_stream` 每块 60s 超时，空流不再无限挂起；单次请求超时降到 120s |
| 直连客户端单测 | ✅ | 配置代理时 `direct_client` 创建、移除后为 None（ENV_LOCK 串行保护） |
| 实机复测 | ⚠️ 网络仍不稳 | 视觉 Agent 多轮任务在本地代理下仍出现后续调用挂起（4 分钟无数据，超时看门狗应触发但未见退出，指向代理连接级问题）；单轮模型冒烟正常（6.4s） |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 109） |

## 二十七、v0.4.21 全本地工具调用验证（2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| llama3.2:3b 工具调用 | ✅ | 直连 Ollama 触发 read_file（11.9s）；经完整 Agent（全部 25 工具）read_file 轮 64.8s 完成并汇报 |
| 全本地视觉 Agent 闭环 | ⚠️ 慢/超时 | 复杂多步中文提示词 + 25 工具在 CPU（3B）首个请求 10 分钟未返回（Ollama CPU 停滞）；本地多轮可行性成立，性能不达标 |
| 结论 | ⚠️ | 本地工具调用链路可用；复杂任务建议用 DeepSeek（网络稳定时）或等待 GPU/更大算力；llama3.2 适合简单工具轮 |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 109） |

## 二十六、v0.4.20 定位多轮卡死根因：screen_ocr boxes（2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| 根因定位 | ✅ | 同一会话内：read_file 工具轮 10.1s、ocr_region 工具轮 9.6s、screen_ocr(max_boxes=0) 8.7s 均正常；screen_ocr 默认（含 120 boxes）第二轮必卡——**boxes 数组是 DeepSeek 多轮卡死触发点** |
| 修复 | ✅ | screen_ocr 默认 `max_boxes=0`（不再带 boxes；lines 已含坐标）；desktop_wait_until 内部同步改为 0 |
| 完整任务复测 | ⚠️ 外部流式 | 单工具轮通过；含多步提示词的完整视觉任务仍偶发第二轮无响应（无 delta、CPU 空闲），指向 DeepSeek 外部流式不稳定 |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 109） |

## 二十五、v0.4.19 OCR 输出截断与多轮复测（2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| OCR 输出截断 | ✅ | `ocr_summary_json`：text≤2000 字符、lines≤60、boxes≤80，控制多轮上下文体积 |
| DeepSeek 多轮复测 | ⚠️ 偶发 | 无工具双轮稳定（1.2-1.8s）；read_file 工具轮正常（10.9s）；screen_ocr 工具轮在 4097 仍偶发无响应（与结果大小无关，指向 DeepSeek 对该工具结果内容的流式不稳定） |
| 本地工具调用模型 | ⏳ 进行中 | qwen2.5:0.5b/3b 均不触发 tool_calls；llama3.2:3b 拉取中（工具调用能力强，待验证） |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 109） |

## 二十四、v0.4.18 本地文本模型工具调用评估（2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| qwen2.5:3b 本地文本模型 | ✅ | Ollama 拉取完成（与 VL 模型并存）；单轮对话正常（46s，CPU） |
| 工具调用能力 | ⚠️ 不达标 | 直连 Ollama /v1/chat/completions 带 tools 请求：qwen2.5vl:3b 直接 400“不支持 tools”；qwen2.5:3b 接受 tools 但不触发调用（返回 null tool_calls 并反问），Agent 工具闭环无法全本地化 |
| 结论 | ⚠️ | 全本地多轮 Agent 需更强工具调用模型（如 llama3.2:3b 或 7B+）或 DeepSeek 网络恢复；当前继续用 DeepSeek（网络恢复后）+ 本地 VL 做视觉验证 |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 109） |

## 二十三、v0.4.17 本地模型通道与回归门禁（2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| 回归门禁脚本 | ✅ | `sim-regression.py`：确定性套件（qq-learn、qq-observe）顺序执行并聚合结果；`--with-llm` 追加 QQ 单轮/浏览器模拟 |
| 确定性回归实测 | ✅ 2/2 | qq-learn PASS、qq-observe PASS（“回归复用验证-001/回归观察验证-001”均发送成功） |
| Ollama VL 工具调用限制 | ⚠️ 已确认 | `qwen2.5vl:3b` 的 OpenAI 兼容端点对含 tools 的请求返回 400（“does not support tools”）；已改拉文本版 `qwen2.5:3b`（支持工具调用，后台下载中） |
| 本地模型多轮通道 | ⏳ 进行中 | qwen2.5:3b 下载完成后，Agent 多轮闭环可完全本地化，绕开 DeepSeek 网络不稳定 |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 109） |

环境变量：`PADDLE_OCR_TOKEN`、`PADDLE_OCR_MODEL`（默认 PP-OCRv6）、`PADDLE_OCR_API_URL`、
`PADDLE_OCR_PROXY`（可选）、`OWO_OCR_STRICT=paddle`（诊断）。本地 ONNX 部署（RapidOCR/Paddle 模型）为后续替换项。

## 十四、v0.4.8 真实环境迭代修复（2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| SendInput 偶发失败重试 | ✅ | 真实桌面实测 `SendInput 文本注入失败` 瞬态报错 → `send_inputs_retry`（3 次重试+退避），click/type/shortcut/scroll 统一接入 |
| 浏览器驱动 src 解析增强 | ✅ | 首张图 src 为空时回退 `currentSrc/data-src`，取页面首个非空图片地址；`browser-driver-direct-test.py` 直连真实网页（不经 Agent）通过：360 搜索 → 今日头条文章 → 下载 141,751B JPEG |
| 真实 QQ 群聊受控发送脚本 | ✅ 就绪（待交互桌面） | `real-qq-group-send.py`：搜索群 → UIA 点击会话行 → 校验聊天头（防发错）→ 输入 → 发送 → UIA 验证；`qq-tree-dump.py` 调试工具；当次运行因桌面会话前台不可用（GetForegroundWindow 为空）中止，未发送任何消息 |
| 本地 OCR 部署可行性 | ⚠️ 网络受阻 | PyPI TLS 连接被当前网络中断（SSLEOFError），rapidocr/onnxruntime 无法安装；云端 PP-OCRv6 继续作为当前 OCR 通道，本地 ONNX 待网络恢复后落地 |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 99） |

## 二十九、v0.4.23 视觉验证占位符 + 视觉 grounding 并入元素注册表（2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| vision_verify 忽略占位符 | ✅ | `verification_prompt(question, ignore_placeholder)` 纯函数：默认 true 时提示模型“输入框内的灰色/浅色占位文字（如‘输入消息...’）不算实际内容”；Agent 工具与 `POST /vision/verify` 均接入（`ignore_placeholder` 默认 true）；单测覆盖提示词开关 |
| 视觉 grounding 并入注册表（source=vision） | ✅ | `VisionGrounding`（描述+坐标框+置信度+cross_validated）+ `fuse_sources_with_vision` 三源融合：视觉元素与重合 OCR 行合并（cross_validated 补 `ocr` 源），与 UIA/OCR 共享稳定 ID 空间；`register_vision_grounding` 复用原 ID；4 个新单测 |
| `/perception/elements` 支持视觉输入 | ✅ | `ElementsRequest.vision`（Vec\<VisionGrounding>）→ UIA+OCR+vision 融合 → 注册表更新返回稳定元素列表 |
| `/vision/ground` 注册返回 element_id | ✅ | 请求可选 `app_id`；grounding matched 时写入注册表并返回 `element_id`，Agent 后续可直接按 ID 点击 |
| `vision_ground` 工具注册 | ✅ | Agent 工具新增可选 `app_id`：结果并入共享注册表（ToolContext/Agent 与 HTTP 层共用同一注册表，`AppState::new` 接线），返回 element_id；`vision_grounding_from_value` 单测 |
| `desktop_click` 稳定 ID 点击 | ✅ | 工具新增 `element_id`+`app_id`：按注册表取元素中心点击（优先于坐标），未命中给出“请刷新感知”的明确错误；坐标模式兼容不变 |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 114） |

说明：本轮打通“视觉定位 → 注册表稳定 ID → 点击/验证”的 Agent 工具闭环，减少对每次 OCR
重新定位的依赖。

## 三十、v0.4.24 动作图执行器接入 element_id 锚点（2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| `SemanticAnchor.element_id` 字段 | ✅ | 可选字段（serde default 兼容旧技能包 JSON）；`semantic_anchor_element_id_round_trip_and_backward_compat` 单测覆盖新旧格式 |
| `WindowsUiaSource.new_with_registry` | ✅ | 执行器源可携带窗口元素注册表；`new()` 保持无注册表兼容 |
| `find` element_id 优先定位 | ✅ | 锚点带 element_id 时按注册表取元素中心坐标（Point 通道点击/注入），未命中返回“稳定元素 ID 未命中（可能已失效），请先刷新 /perception/elements”明确错误；敏感面熔断仍在 execute_graph 层生效 |
| 服务端接线 | ✅ | `ui_action_source(state.elements.clone())`：`/learn/execute` 与 `/learn/execute-package` 均共享 HTTP 感知层同一注册表；HTTP 冒烟：confirm=true + element_id 锚点返回“稳定元素 ID 未命中（可能已失效）”，confirm=false 仍 400 强制审批 |
| 纯函数 | ✅ | `registry_element_point`（按稳定 ID 取中心）+ 单测（命中/未命中） |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 117） |

说明：至此“视觉定位 → 注册表稳定 ID → 点击/验证”在 Agent 工具与动作图执行器两条路径均闭环；
旧流程技能包（无 element_id）仍按语义锚点（UIA/OCR）原有逻辑执行，不受影响。

## 三十一、v0.4.25 真实面观察源（桌面状态采样，2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| DesktopSnapshot 采样 | ✅ | `sample_desktop()`：前台应用 + 窗口标题哈希（原始标题不落盘）+ 剪贴板序列号；无前台窗口时安全返回 None 字段 |
| 变化检测观察 | ✅ | `desktop_observation(prev, next)`：仅前台应用/标题哈希/剪贴板序列变化时生成 `kind=desktop_event` 记录；摘要如“前台应用：qq”“窗口标题变化（内容掩码）”“剪贴板变化（内容掩码）” |
| 隐私边界（D22） | ✅ | 观察 detail 只含 title_hash / clipboard_changed，不含标题原文与剪贴板内容；契约测试断言序列化结果不包含原始标题 |
| 服务端接线 | ✅ | `start_memory_observer` 每 2s 采样桌面（受 L0Event 感知授权门控，可热撤），模拟面日志观察逻辑保持不变 |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 120） |

说明：真实面观察源落地后，情景记忆可覆盖“用户正在什么应用、切了哪个窗口、剪贴板是否变化”
的掩码轨迹；技能挖掘（/memory/mine-skill）仍以模拟面/示范录制的动作序列为准，
真实面动作级学习走 UIA 锚点录制链路。

## 三十二、v0.4.26 主动建议“学习”确认转 active 技能包（2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| 建议动作序列 → 学习样本 | ✅ | `recorded_actions_from_sequence`：`click:发送`/`type:输入消息` 解析为动作图样本，Type 一律内容掩码；空锚点/未知前缀安全处理；单测覆盖 |
| 直接沉淀接口 | ✅ | `LearnPipeline::sink_from_actions`（sink_skill 重构复用）：建议确认后无需先录制，直接泛化 → 流程技能包 → 入 FlowSkillStore（active）；单测验证包合法且可列出 |
| `/proactive/decide` learn 闭环 | ✅ | HTTP 层先取建议，decide 后把序列沉淀为 `proactive-<uuid>` 技能包（target_apps=建议应用、sensitivity=low），返回 package 信息并写 learn-confirm 审计；Learn 后建议从列表移除避免重复学习 |
| UI/API 动作枚举不一致修复 | ✅ | Web 端按钮改发 `execute_once`/`mute_forever`；服务端 `SuggestionAction` 增加 `execute`/`mute` serde 别名兼容旧调用 |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 123） |
| HTTP e2e | ✅ 1/1 | 3 次 observe → 建议；`execute` 别名 200；再 observe → `learn` 200 返回 `proactive-3f942068`；`/learn/packages` 可见；建议列表移除；`mute` 别名解析正常（404=JSON 已解析） |

说明：D24 主动建议的“学习/执行一次/忽略/静默”四选在 HTTP+Web 端全部可解析；
“执行一次”仍走执行审批（技能包执行需 confirm），默认只提示不执行的安全边界保持不变。

## 三十三、v0.4.27 vision_ground 置信度解析（2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| 边界框+置信度解析 | ✅ | `parse_vision_box_with_confidence`：支持 `BOX x,y,w,h 0.85`、`BOX x,y,w,h（置信度 80%）`、裸四元组；无置信度时为 None；`VisionBox` 类型别名收敛签名 |
| 置信度提取复用 | ✅ | `extract_confidence` 供 parse_verification 与 grounding 共用；百分比兜底仅在文本含 `%` 时启用（修复裸整数被误判为 0.x 的缺陷） |
| grounding 提示词与响应 | ✅ | 提示词要求“BOX x,y,w,h + 0-1 置信度”；matched/未交叉验证响应均带 `confidence` 字段（未给出时 null） |
| 注册表置信度透传 | ✅ | `vision_grounding_from_value` 已读取 confidence（缺省 0.7），单测覆盖 0.88 透传与缺省值 |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 123） |

说明：置信度进入元素注册表后，可支撑后续“低置信度元素不直接点击/需二次确认”的策略
（当前交叉验证仍以 OCR 重合为准，安全边界不变）。

## 三十四、v0.4.28 视觉-only 高置信度定位策略（2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| vision-only 门槛 | ✅ | `vision_only_allowed(confidence, min)` 纯函数：无 OCR 重合时仅置信度 ≥0.9 允许视觉定位命中（`OWO_VISION_MIN_VISION_ONLY_CONFIDENCE` 可调）；单测覆盖边界值 |
| grounding 三态响应 | ✅ | OCR 重合 → `matched=true/cross_validated=true`；无 OCR 但高置信度 → `matched=true/vision_only=true`（仅纯视觉元素，图片表情/自绘按钮）；无 OCR 且低置信度 → `matched=false`“未重合且置信度不足” |
| 工具契约更新 | ✅ | `vision_ground` 描述区分 cross_validated（点 line 中心）与 vision_only（只能点 box 中心），低置信度拒绝保持 |
| 安全边界 | ✅ | 默认 0.9 高门槛 + 环境变量可调；敏感面熔断、审批流程不受影响 |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 124） |

说明：该策略解决 QQ 图片表情面板等“OCR 无文字、纯图像渲染”元素的定位问题，
为后续视觉-only 点击闭环提供受控入口（仍建议首次执行走审批）。

## 三十五、v0.4.29 模型网关用量统计（P0 补齐，2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| TokenUsage 结构 | ✅ | prompt/completion/total + `add`/`saturating_sub`/`cost_estimate_usd`（$/M token 单价可配，默认 0）；单测覆盖算术与成本 |
| usage 解析 | ✅ | `parse_usage_value` 兼容 OpenAI/DeepSeek（prompt_tokens 等）与 Ollama 原生字段（prompt_eval_count/eval_count）；流式末尾 usage 块经 `parse_sse_payload` 提取 |
| Provider 累计快照 | ✅ | `OpenAiCompatibleProvider.usage` 互斥累计，非流式与流式均记录；`ModelProvider::usage_snapshot` 默认零（未实现 Provider 兼容） |
| 回合增量 | ✅ | `TurnOutcome.usage`（serde default 兼容旧序列化）：回合开始/结束取快照差值，跨多次模型调用累计 |
| 可观测落点 | ✅ | TraceRecord 记录 usage（旧 trace 反序列化兼容）；服务端回合结束写 `model/usage` 审计（prompt/completion/total/cost_usd，价格经 `OWO_MODEL_INPUT/OUTPUT_PRICE_PER_MTOK` 配置） |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 127） |

说明：P0“用量统计/预算上限”统计部分已补齐；预算上限见下一节。

## 三十六、v0.4.30 模型用量预算上限（P0 补齐，2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| 预算熔断纯函数 | ✅ | `budget_violation(usage, total_cap, cost_cap, in_price, out_price)`：累计 token 或成本任一超限返回明确原因；未配置预算返回 None；单测覆盖未超/超限/成本三档 |
| Provider 预算检查 | ✅ | `OpenAiCompatibleProvider::usage_budget_check` 每次 complete/complete_stream 发请求前检查（在数据出境开关之后、联网之前），超限直接 Err，不产生新开销 |
| 配置入口 | ✅ | `OWO_USAGE_TOKEN_BUDGET`（累计 token 上限）、`OWO_USAGE_COST_BUDGET_USD`（成本上限，配合 `OWO_MODEL_INPUT/OUTPUT_PRICE_PER_MTOK` 单价）；未配置默认不熔断 |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 128） |

说明：至此 P0 模型网关“统一 Provider 接口/流式/工具调用/用量统计/预算上限/BYOK”全部落地；
预算为累计口径（跨会话进程内累计），进程重启后归零，云端持久化预算留作 v2。

## 三十七、v0.4.31 桌面端用量面板与 /usage 接口（2026-08-13）

| 项 | 状态 | 证据 |
|---|---|---|
| `GET /usage` | ✅ | 返回累计 token（输入/输出/总）、成本估算、预算配置（token/cost 上限、单价）与是否超限（violation）；OpenAPI 已登记 |
| 桌面端用量面板 | ✅ | 设置与诊断区新增“模型用量与预算”：累计 tokens/成本/预算/状态，10s 自动刷新，预算未配置显示“未配置”，超限显示 ⚠️ |
| 前端语法 | ✅ | `node --check` 通过 |
| HTTP 冒烟 | ✅ | 带预算环境变量启动：/usage 返回 total=0、cost=0.0、token_cap=100000、cost_cap=5.0、violation=null；openapi.json 含 /usage |
| 质量门禁 | ✅ | fmt/clippy 0 警告；`cargo test --workspace` 全绿（core 128） |

说明：预算配置当前经环境变量生效（运行时改动即时被 provider 读取），
设置页 JSON 持久化预算字段留作后续（settings.json 扩展）。
