# STATUS-B.md — Agent B 状态（桌面工作台前端联调）

> 我只写本文件。任务来源：主控 2026-08-14 分工指令。

## 认领

- 时间：2026-08-14
- 任务：让 `desktop/web` 工作台所有面板与 Agent A 恢复后的 HTTP 契约对齐，消除 404 白屏，错误处理友好。
- 白名单文件：`desktop/web/index.html`、`desktop/web/app.js`、`desktop/web/style.css`。
- 依赖：A 发布 `.coord/CONTRACT.md` 后开始联调；此前先做纯前端检查。

## 面板 → 接口矩阵（app.js 现状，2026-08-14 13:3x）

| 面板 | 调用接口 | 状态 |
| --- | --- | --- |
| 健康 | GET /health | 待 A 契约 |
| 插件管理 | GET /plugins、POST /plugins/{id}/enabled | 待 A 契约 |
| 流程技能包 | GET /learn/packages、GET /learn/packages/{name}、DELETE /learn/packages/{name}、POST /learn/execute-package、GET /learn/export/{name}、POST /learn/import | 待 A 契约 |
| 技能管理 | GET /skills、POST /skills/{name}/enabled、GET /skills/{name}、保存 /skills/{name} | 待 A 契约 |
| 技能健康 | GET /skills/health、POST /skills/health/{name}/reset | 待 A 契约 |
| 记忆观察 | GET /memory/observations?limit=30 | 待 A 契约 |
| 记忆检索 | GET /memory/recall?q=&top_k=8 | 待 A 契约 |
| Traces | GET /traces、GET /traces/{index} | 待 A 契约 |
| 子代理 | POST /subagent/run | 待 A 契约 |
| 项目规则 | GET /project/rules、POST /project/rules、POST /project/rules/template | 待 A 契约 |
| MCP 管理 | GET /mcp、POST /mcp/add、POST /mcp/remove | 待 A 契约 |
| 上下文仪表 | GET /session/{id}/context | 待 A 契约 |
| 模型用量 | GET /usage | 待 A 契约 |
| 审计 | GET /audit | 待 A 契约 |
| 自动化 | GET /automations、POST /automations、POST /automations/{id}/toggle、DELETE /automations/{id}、GET /automations/reminders、POST /automations/reminders/clear | 待 A 契约 |
| 设置 | GET /settings、PUT /settings、POST /settings/egress | 待 A 契约 |
| 学习 | GET /learn/status、POST /learn/{action}、POST /learn/sink | 待 A 契约 |
| 会话 | GET /sessions、POST /session、GET /session/{id}、POST /session/{id}/turn、/rename /pin /archive /fork /rewind /redo /abort、GET /session/{id}/diff、POST /session/{id}/revert、POST /session/{id}/permission/{requestId}、GET /session/{id}/export/{format}、POST /session/{id}/attachments | 待 A 契约 |
| 白名单 | GET /whitelist | 待 A 契约 |
| STT | POST /stt/transcribe | 待 A 契约 |
| Eval | POST /eval/run | 待 A 契约 |

## 执行记录（时间戳）

- 13:2x 认领登记完成；D 已冻结 OWNERSHIP.md（B 域 = desktop/web/*）。
- 13:3x 纯前端检查完成：
  - `node --check app.js` 0 错误（基线通过）。
  - XSS 审计通过：所有 innerHTML 插值经 `esc()` 转义；`renderMarkdown` 输入先 escapeHtml，链接经 `safeMarkdownHref` 协议白名单（http/https/mailto/相对路径），无新增注入点。
  - **发现**：多数面板 catch 静默显示"读取失败"（无状态码），上下文仪表失败时直接隐藏（白屏风险），不符合"404/500 显示服务接口不可用"。
- 13:4x 前端错误处理改进（不依赖契约，纯前端）：
  - 新增 `friendlyError(error)`：404/405/5xx → "服务接口不可用（HTTP x）"，其余透传。
  - 更新 18 处面板 catch：插件、技能健康、Traces、项目规则、MCP、上下文仪表（不再隐藏，显示错误）、用量、流程技能包、自动化、提醒、设置、建议、审计、技能管理、记忆观察、白名单、学习。
  - `node --check` 0 错误；`esc()` 包裹所有 innerHTML 错误文案，无新增 XSS。
- 13:4x 与快照（clients/ts/openapi.json 08-13，73 路径）核对：app.js 调用路径/方法/请求体与快照 schema 兼容（/settings POST、/skills/{name} GET+POST、/mcp/add {name,transport,command,url,args}、/subagent/run {prompt,read_only,model}、/plugins/{id}/enabled {enabled}、/project/rules {content} 等）。⚠️ `/memory/recall` 不在快照中（08-13 后新增），需 A 契约确认。
- 14:0x P2 computer-use 任务面板完成（index.html + app.js + boot 轮询 15s）：列表/创建/任务级批准/拒绝/暂停/恢复/终止，端点按文档 7.3 语义 + 1292 行形态 `/computer-use/tasks|task|task/{id}/{action}`；字段名（app/description/max_duration_secs/allowed_actions、task.id/state/elapsed_ms）标注"待 A 契约确认"。
  `node --check` 0 错误。
- 14:1x 静态完整性检查：app.js 引用 97 个 DOM id 全部存在于 index.html（无 null 引用白屏风险）；未引用仅布局容器（sidebar/right/contextMeter）。
- 14:1x 预写接口矩阵验证脚本（verify_matrix.py，temp 目录）：21 面板 27 接口，合法输入取 2xx、最小非法输入取 400/422，避免副作用（不 spawn MCP 进程/不跑真实模型），输出 PASS/FAIL 矩阵。待 A 契约发布+服务可起后执行。
- 待填写：CONTRACT.md 对齐、接口矩阵实测（非 404）、P2 computer-use 字段确认。

## 遗留问题

- A 尚未发布 CONTRACT.md；发布后需逐面板核对路径/方法/请求体/字段名，并留言确认 `/memory/recall` 与 computer-use 字段。
- P2 computer-use 面板字段名待 A 契约确认（当前按文档语义实现）。
