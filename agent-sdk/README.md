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
- 设置组：`stt` / `explore` / `proactive` / `skills` / `whitelist` / `egress`（参考 `settings.example.json`）。
- 桌面工作台 Web 壳（P1 骨架）：`desktop/web/`（任务列表/对话 SSE 流式/审批条/diff 审阅/技能中心/感知状态区/白名单管理），由 HTTP 服务在 `/` 静态托管，后续用 Tauri 2 封装为桌面主客户端。
- L0 前台窗口事件源（Windows）：`platform.rs` 用 Win32 轮询前台应用（app_id + 标题），`/context/snapshot` 自动刷新并去重写入情景快照。
- L0 剪贴板事件源：轮询 `GetClipboardSequenceNumber`，只记录“内容已变化”（掩码），不读取/不保存剪贴板内容。
- L2 按需截图：GDI `BitBlt` + `GetDIBits` 抓屏为内存 BMP，进环形缓冲（最多 5 帧、不落盘），任务结束 `discard_captures` 即毁；快照只暴露元数据（大小/时间），不暴露像素。
- L1 无障碍 UI 树（Windows UI Automation）：`accessibility.rs` 抓取前台窗口语义锚点（角色/名称/类名，按深度与节点数截断），写入情景快照 `ui_context.ui_tree`，内容未变化不重复记事件。
- L2 本地摘要（Windows OCR）：`ocr.rs` 用系统自带 Media.Ocr 对内存截图离线识别文字，摘要进环形缓冲帧元数据（不落盘）；`POST /perception/capture` 按需采集（可传 width/height 采样），`POST /perception/layers` 逐层授权/热撤（L2 默认关闭，拒绝时 400）。
- P3 动作图执行引擎（`executor.rs`）：按流程技能包动作图执行——语义锚点定位（UI Automation）→ 点击/输入/快捷键（SendInput）→ 状态验证；敏感面（密码/支付/验证码）熔断、成环检测、步数上限；`POST /learn/execute` 提交 `{graph, variables, max_steps}` 返回分步执行报告。
- P3 示范学习流水线（`learn.rs`）：录制样本 → 泛化为动作图（同锚点重复 Type 推断 `{value}` 变量）→ 沉淀流程技能包（SKILL.md + graph.json + manifest.json）；`/learn/execute` 每步写入审计。
- P3 桌面闭环 UI（`desktop/web`）：操作学习面板（开始/暂停/恢复/结束/清空录制、沉淀技能包、流程技能包列表与一键执行）+ 主动建议区（学习/执行一次/忽略/静默 四选）；接口 `/learn/start|stop|packages|sink|execute-package`、`/proactive/suggestions`。
- P3 执行审批：`/learn/execute` 与 `/learn/execute-package` 服务端强制 `confirm: true`（首次执行必须确认），确认与分步结果写入审计；录制自动观察（`start_observer`）在录制中每 2s 采样前台/剪贴板掩码事件（前台变化去重、剪贴板按序列号去重）。
- P3 高敏感二次确认：`sensitivity=high` 的流程技能包执行还需 `high_risk_ack: true`，否则 400；确认写入审计。
- 自动化面板（P1）：`automation.rs` 定时任务（单次/间隔/每天）+ 提醒动作，持久化 `<data>/automations.json`，常驻循环每秒检查、触发写审计；接口 `GET/POST /automations`、`POST /automations/{id}/toggle`、`DELETE /automations/{id}`、`GET /automations/reminders`、`POST /automations/reminders/clear`；Web 工作台自动化面板（创建/启停/删除/提醒列表）。
- 流程技能包分享（D26）：`share_skill.rs` 导出/导入 `.owskill`（ZIP，含 SKILL.md/graph.json/manifest.json/versions.json）；导入校验顺序为 schema → 权限白名单（默认 deny）→ 敏感度必填 → 变量/动作图合法，zip-slip 拒绝；接口 `GET /learn/export/{name}`、`POST /learn/import`（raw ZIP），Web 工作台支持导出/导入。
- 语音输入（本地优先）：Web 工作台 🎤 按钮用 WebAudio 采集麦克风 → 16k WAV → `POST /stt/transcribe`（SenseVoice-Small 本地推理）→ 转写进输入框；本地 STT 不可用（模型缺失/权限拒绝）时自动回退系统 Web Speech；最长录 10 秒自动停止。
- 数据出境开关（7.5）：`settings.egress.cloud_enabled`（默认开，可用 `OWO_CLOUD_ENABLED=false` 覆盖）；关闭后模型网关在发起任何网络请求前直接拒绝（完整/流式两条路径），HTTP `GET /settings` / `POST /settings/egress` 读写，Web 工作台“设置与诊断”区一键切换；写回 `settings.json` 并即时生效（网关每次调用前检查运行时开关），切换记入审计，无需重启。
- 设置与诊断（P1）：`GET /settings` 读取、`POST /settings` 保存完整 `settings.json` 并即时应用运行时设置（数据出境、模型热切换——新回合生效、STT 模型/语言/ITN、主动建议阈值、白名单合并默认清单），保存写审计；`whitelist/manage` 同步持久化用户清单；Web 工作台“设置与诊断”区含 JSON 编辑器 + 保存按钮 + 数据出境开关 + OpenAPI 链接。
- 会话管理（P1）：会话新增 `title` / `archived` / `pinned`（SQLite 自动迁移），`GET /session/{id}` 返回历史消息支持断点恢复，`POST /session/{id}/rename|archive|pin` 修改元数据，列表按置顶 + 更新时间排序、归档默认隐藏；Web 工作台“任务”区为会话树（子会话缩进展示），每个会话可继续/重命名/置顶/归档/fork/回退/重做。
- 审计落库与日志（P1）：`SessionStore` 提供 `append_audit` / `recent_audit`（SQLite 落库，条目自带 session_id）；服务端回合结束后与设置/学习等操作的内存审计统一 flush 到 SQLite；`GET /audit?limit=N` 返回最近审计；Web 工作台右侧“审计日志”面板 5s 刷新。
- 技能中心（P1）：技能启用/禁用（运行时共享禁用集合，切换即时生效并持久化到 `settings.json`；系统提示注入与 `use_skill` 均只放行启用技能）；`GET /skills/{name}` 查看、`POST /skills/{name}` 编辑 SKILL.md；`GET /learn/packages/{name}` 流程技能包详情、`DELETE /learn/packages/{name}` 删除（写审计）；Web 技能中心含启用/禁用、查看、编辑、导出、删除按钮。
- 对话附件（P1）：`POST /session/{id}/attachments` 上传（base64 JSON、文件名清洗防穿越、50MB 上限、保存到工作区 `.owo-attachments/<会话>/`），`GET /session/{id}/attachments` 列表；`TurnRequest.attachments` 发送时自动注入附件路径上下文（Agent 可用内置文件工具读取）；Web 📎 多选上传 + 附件 chips（可移除）。
- 桌面操作迭代（P3/computer-use）：动作图新增 `launch`（主动打开应用/URL）与 `click_at`（按屏幕坐标点击，配合 OCR 定位自绘控件）；修复 `inject` handle=0 失效；新增 `POST /perception/tree`（深度树，节点含屏幕边界框）、`POST /perception/ocr`（全屏 OCR + 逐词坐标框）、`POST /perception/ocr/region`（裁剪+放大区域 OCR，小字验证窗口用）、`GET /perception/ocr/status`（引擎诊断）；修复 OCR 根因：WIC 无法解码 GDI BMP，改为直接构造 SoftwareBitmap（实测 1636 字符/647 框）；`SemanticAnchor.parent` 父容器约束；`ui:`/`value:` 验证谓词；`qq-send-file` 技能包按 NTQQ 实测锚点重写。
- 桌面自启：Tauri 托盘新增“开机自启：开/关”，写入/删除 HKCU Run 注册表项，启动时自动拉起核心服务常驻。
- 本地 STT（D20）：`stt.rs` 集成 sherpa-onnx + SenseVoice-Small（默认离线，`settings.stt.model` 可换），模型目录 `<data>/models/stt/<model>/`，`scripts/download-stt-model.ps1` 一键下载（约 240MB）；接口 `POST /stt/transcribe`（raw WAV → 文本 + 耗时）；模型未就绪返回明确错误，不静默降级云端。
- 桌面/浏览器双表面工具（v0.4.1）：`screen_ocr`/`ocr_region` 返回整行文本（`lines` + 坐标 + `role_hint`），
  `desktop_click/type/key/shortcut/activate/window_list/foreground/launch/wait` 走 SendInput/UIA/窗口枚举，
  `desktop_wait_until` 按 OCR 谓词轮询等待（等对方回复/消息上屏，可限定 role_hint），
  `browser_navigate/search/snapshot/click/type/press/screenshot/download_image/close` 走 Playwright + 本机 Edge
  （持久化 profile、可 headless、支持 `OWO_BROWSER_PROXY` 代理）；设置 `OWO_SIM_QQ_URL=http://127.0.0.1:18500` 后，桌面工具自动落到
  headless 模拟窗口（离屏渲染 + HTTP 虚拟输入），完全不碰真实桌面；服务端直连写接口在模拟面下被禁用。
- 模拟实验台（`sim/`）：`owo-sim-qq --headless` 提供自绘 QQ 聊天窗口的 `/frame`（BMP）、`/ocr`（真值版面）、
  `/click`、`/type`、`/key`、`/state`、`/log`、`/reset`；`owo-sim-browser` 提供本地搜索/文章/图片下载站；
  附带多联系人场景（`sim/scenarios/qq-multi-contact.json` + 对应提示词）；
  `scripts/run-sim-e2e.ps1` 一键跑 QQ 回复闭环 + 浏览器搜索/下载两个端到端验收（后台静默，不弹窗）；
  `scripts/web-browser-e2e.py` 可对真实网页（headless Edge）跑搜索→打开结果→下载图片验收；
  `scripts/sim-qq-learn-e2e.py` 演示“示范→录制（内容掩码）→泛化→沉淀技能包→换参数复用执行”闭环
  （`/learn/execute*` 在模拟面自动走 SimUiActionSource，不碰真实桌面）。
- 视觉模型网关（v0.4.4，M-B 起步）：`screen_vision`（场景描述）与 `vision_verify`（yes/no 完成验证）工具，
  通道为本地 Ollama（默认 `qwen2.5vl:3b`，`scripts/download-vision-model.ps1` 一键拉取）或任意
  OpenAI-compatible 视觉端点（`OWO_VISION_PROVIDER=openai` + `OWO_VISION_BASE_URL/API_KEY/MODEL`）；
  `GET /vision/status` 诊断、`POST /vision/describe` 直连测试；模型未就绪时返回明确错误不挂起。
- 静默观察与情景记忆（v0.4.5，M-D 起步）：服务启动即挂载观察器（模拟面每 2s 拉取应用事件流），
  动作摘要（内容掩码）写入本地情景记忆 `memory.jsonl`；`GET /memory/observations` 查看、
  `POST /memory/mine-skill` 一键把观察序列挖掘为流程技能包（复用 LearnPipeline 泛化）；
  `scripts/sim-qq-observe-e2e.py` 演示“静默观察→挖掘→换参数复用执行”闭环。
- BYOK 视觉通道已验证（v0.4.6）：`scripts/vision-mock-e2e.py` 用 mock OpenAI-compatible 端点
  验证 `screen_vision`/`vision_verify`/`/vision/describe`/`/vision/verify` 全链路（含图片 payload）；
  本地 VL 模型就绪后 `scripts/sim-qq-vision-e2e.py` 可跑真实视觉描述/验证。
- PP-OCRv6 OCR（v0.4.7，M-A 主力路径）：`PADDLE_OCR_TOKEN` 启用后，screen_ocr/ocr_region/
  `/perception/ocr*` 自动走 PP-OCRv6（失败回退 Media.Ocr，provider 字段标注引擎）；实测能读出
  Media.Ocr 读不出的离屏小字（“发送/输入消息”）。`scripts/real-qq-send.py` 用 UIA 锚点在真实 QQ
  完成受控发送并验证上屏；`desktop_scroll` 支持滚轮滚动列表。
- 真实环境迭代工具（v0.4.8）：`scripts/browser-driver-direct-test.py` 不经 Agent 直连真实网页
  验证驱动（360 搜索→文章→下载 141KB JPEG）；`scripts/real-qq-group-send.py` 群聊受控发送
  （带“校验聊天头防发错”保护）；SendInput 注入失败自动重试。
- 窗口级截取（v0.4.9，M-A）：`POST /perception/window {hwnd}` 与 `desktop_window_ocr` 工具
  用 PrintWindow 后台只读抓指定窗口并用 PP-OCRv6 识别（返回屏幕坐标）；实测后台抓 QQ 窗口
  339 字符/33 行，不切前台。
- 窗口模板（v0.4.10，M-A）：`/perception/template/build[-ocr]|detect[-ocr]` 从 UIA 树或 OCR 版面
  提取/检测“发送/输入框/搜索”等 ROI 并持久化；`capture_window_bmp_deep` 枚举子窗口择优抓帧。
  注意：锁屏时 Chromium 应用（QQ）不向窗口 DC 呈现底部输入区，完整窗口内容需交互桌面会话。
- 真实桌面输入注意（v0.4.11）：沙箱/提权子进程没有交互输入桌面（SetCursorPos 0x800700CB），
  真实 QQ 等应用的鼠标键盘注入需要用户在交互会话启动核心服务；`scripts/owo-session-probe.ps1`
  可验证桌面可达性。
- 窗口元素注册表（v0.4.12，10.1）：`/perception/elements` 融合 UIA+OCR 为稳定元素列表
  （`SceneElement`：稳定 ID/多源/置信度/stale 淘汰）；实测 QQ 窗口连续两帧 34/34 稳定 ID。
- 执行器 OCR 锚点兜底（v0.4.13，4.1 L2）：动作图执行时 UIA 找不到锚点会自动用屏幕 OCR
  定位文本中心再点击/输入（`find_ocr_anchor_point` 纯函数 + 单测）。
- 本地视觉模型已就绪（v0.4.14）：Ollama `qwen2.5vl:3b`（约 3GB），`screen_vision`/`vision_verify`
  支持区域裁剪放大；实测正确描述模拟 QQ 界面并以 yes/0.8 验证“输入框清空/消息上屏”。
- 情景记忆滚动清理（v0.4.15）：默认 30 天/1 万条（`OWO_MEMORY_RETENTION_DAYS`/`OWO_MEMORY_MAX`），
  加载与追加时自动清理；`scripts/model-smoke.py` 可快速验证模型通道。
- 模型网关韧性（v0.4.16）：请求失败自动代理→直连降级，流式每块 60s 空闲看门狗；
  缓解多轮流式挂起（代理网络恢复后即受益）。

### STT WER/CER 评估

```powershell
# 清单：每行 `<wav路径>TAB<标准文本>`（UTF-8），例如 dist\stt-eval.tsv
powershell -ExecutionPolicy Bypass -File scripts\eval-stt-wer.ps1 `
  -Manifest dist\stt-eval.tsv -OutJson dist\stt-eval-report.json
```

评估工具自动启动核心服务、逐条调 `/stt/transcribe`、计算字符级 CER（中文）与词级 WER（英文），输出聚合报告 JSON。

### VSCode 语音改代码 E2E

```powershell
# 需要：DeepSeek/OpenAI 兼容密钥、本地 SenseVoice 模型、VSCode
$env:OPENAI_API_KEY="<key>"; $env:OPENAI_BASE_URL="https://api.deepseek.com/v1"
$env:OPENAI_MODEL="deepseek-v4-flash"; $env:OWO_HTTP_PROXY="http://127.0.0.1:7897"
owo-agent serve --port 4097 --workspace .          # 另开终端
python scripts\voice_code_e2e.py                   # 见脚本内环境变量（E2E_*）
```

完整链路：TTS/真实语音 → `/stt/transcribe`（SenseVoice）→ `/session/{id}/turn`（Agent 读文件→改文件→跑测试）→ 审批自动放行 → 文件验证。

批量跑分（成功率口径）：

```powershell
$env:E2E_ROUNDS = "20"   # 默认 10
python scripts\voice_code_batch.py   # 输出 summary: N/N = xx%
```

### 便携打包

```powershell
powershell -ExecutionPolicy Bypass -File scripts\package-desktop.ps1 -Configuration release
# 产物：dist\OwO-Agent-release.zip（owo-agent.exe + owo-agent-desktop.exe + skills/ + README）
```

便携包内桌面壳自动定位同目录核心服务与 `skills/`（也可用 `OWO_SKILLS_DIR` / `OWO_AGENT_DATA` 覆盖）。

### NSIS 安装程序

```powershell
powershell -ExecutionPolicy Bypass -File scripts\build-installer.ps1
# 产物：desktop\tauri\src-tauri\target\release\bundle\nsis\OwO Agent_0.1.0_x64-setup.exe
```

安装包通过 Tauri externalBin 内置核心服务（`owo-agent-x64.exe`），桌面壳自动定位同目录核心服务；支持简体中文/英文安装界面、当前用户安装。

### 自动更新（updater）

```powershell
powershell -ExecutionPolicy Bypass -File scripts\generate-update-manifest.ps1 `
  -SetupExe dist\OwO-Agent-0.1.0-setup.exe -Version 0.1.0 -BaseUrl https://example.com/owo/updates
# 产物：dist\updates\latest.json（version/notes/pub_date/platforms.windows-x86_64.url+signature）
```

桌面托盘“检查更新”调用 tauri-plugin-updater；签名公钥已内置，私钥在 `desktop/tauri/src-tauri/.secrets/`（gitignore，请妥善保管）。把安装包与 `latest.json` 托管到任意静态服务器并替换 `tauri.conf.json` 的 `plugins.updater.endpoints` 即可启用更新。

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
OWO_CLOUD_ENABLED=false                         # 关闭云端模型调用（settings.json egress 同样生效）
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
