# STATUS-B.md — Agent B 状态（桌面工作台前端联调）

> 我只写本文件。任务来源：主控 2026-08-14 分工指令（Agent B 角色）。

## 认领

- 时间：2026-08-14
- 任务：让 `desktop/web` 工作台所有面板与 Agent A 恢复后的 HTTP 契约对齐，消除 404 白屏，错误处理友好。
- 白名单文件：`desktop/web/index.html`、`desktop/web/app.js`、`desktop/web/style.css`。

## 面板 → 接口矩阵（实测 2026-08-14 15:2x，服务 owo-agent.exe serve --port 4197）

| 面板 | 接口 | 实测 |
| --- | --- | --- |
| 健康 | GET /health | 200 |
| 插件管理 | GET /plugins、POST /plugins/{id}/enabled | 200 / 404（插件不存在，资源型）|
| 流程技能包 | GET /learn/packages、GET /learn/status | 200 / 200 |
| 技能管理 | GET /skills | 200 |
| 技能健康 | GET /skills/health、POST /skills/health/{name}/reset | 200 / 400 |
| 记忆观察 | GET /memory/observations?limit=30 | 200 |
| 记忆检索 | GET /memory/recall?q=&top_k=8 | 200 |
| Traces | GET /traces、GET /traces/{index} | 200 / 404（无 trace，资源型）|
| 子代理 | POST /subagent/run | 422（缺 prompt）|
| 项目规则 | GET/POST /project/rules、POST /project/rules/template | 200 / 200 |
| MCP 管理 | GET /mcp、POST /mcp/remove | 200 / 404（不存在，资源型）|
| 上下文仪表 | GET /session/{id}/context | 404（会话不存在，资源型）|
| 模型用量 | GET /usage | 200 |
| 审计 | GET /audit | 200 |
| 自动化 | GET /automations、GET /automations/reminders | 200 / 200 |
| 设置 | GET /settings | 200 |
| 白名单 | GET /whitelist | 200 |
| Eval | POST /eval/run | 422（缺 suite_id）|
| Computer-use | GET /computer-use/tasks、POST /computer-use/task、POST /computer-use/task/{id}/{action}、POST /computer-use/sensitive-check | 200 / 422 / 400 / 200 |
| 会话 | GET /sessions、GET /session/{id}/diff | 200 / 404（资源型）|
| STT | POST /stt/transcribe | 400 |
| 静态托管 | GET / | 200 |

矩阵结论：**33 项 ALL PASS**（资源型 404 按契约语义通过，无接口丢失）。

## 执行记录（时间戳）

- 13:2x 认领登记；D 冻结 OWNERSHIP.md（B 域 = desktop/web/*）。
- 13:3x 纯前端检查：`node --check` 0 错误；XSS 审计通过（esc() 全覆盖 + renderMarkdown 协议白名单）；发现多数面板静默"读取失败"、上下文仪表失败静默隐藏（白屏风险）。
- 13:4x 错误处理改造：新增 `friendlyError()`（404/405/5xx → "服务接口不可用（HTTP x）"），更新 18 处面板 catch；`node --check` 0 错误。
- 13:4x 契约预核：app.js 调用与 openapi.json 快照（73 路径）schema 兼容；⚠️ /memory/recall 不在快照。
- 14:0x P2 computer-use 任务面板完成（index.html + app.js + boot 15s 轮询）。
- 14:1x DOM id 完整性检查：97 个引用全部存在；矩阵脚本预写。
- 14:58 A 发布 CONTRACT.md（106 路径）。契约对齐修正：
  - computer-use 字段改为契约形态：`target_app`（非 app）、`max_duration_ms`（非 max_duration_secs，UI 秒→内部 ×1000）、拒绝 `reject`/终止 `cancel`（非 abort）；状态前缀匹配（Pending/Running/Paused 大小写兼容）。
  - `friendlyError(error, {resource: true})`：契约资源型 404 路径（会话/技能/包/trace/mcp-remove/automation）显示"资源不存在"而非"服务接口不可用"，10 处 catch 应用。
  - 模板 409 幂等提示：`AGENTS.md 已存在，未生成模板（幂等）`。
  - `node --check` 0 错误。
- 15:2x 实测：起本地服务（4197，临时 workspace），33 项接口矩阵 ALL PASS（资源型 404 白名单按契约语义）；computer-use 契约实测：创建 `{target_app, description, max_duration_ms, allowed_actions}` → 200 `{ok, task:{state:"Pending"}}`；reject → 200 `{ok, state:"Rejected"}`；列表 `{count, tasks}` 与前端读取字段一致。
- 15:3x 测试服务已停止；临时数据目录不落库。

## 产出

- `desktop/web/app.js`：friendlyError（含 resource 模式）、18+10 处面板 catch 统一友好错误、P2 computer-use 面板、409 幂等提示。
- `desktop/web/index.html`：Computer-use 任务 section（P2）。
- `desktop/web/style.css`：未改动（复用现有 .stack/.inline/.list 类）。

## 遗留问题

- P2 computer-use 面板为可选增强，字段已按 A 契约实测对齐；approve 动作未实测（避免真实执行 notepad 等动作，语义由 A 契约测试覆盖）。
- 后续 A 建议的"traces 空态 200 语义"若实施，前端 `GET /traces/{index}` 空态 404 提示可再平滑。
- ⚠️ 编码发现（15:4x）：git HEAD 的 `desktop/web/app.js` 为 **UTF-16LE** 编码（历史 PowerShell 写入遗留，浏览器按 UTF-8 解析会语法错误）；工作区为 B 维护的 **UTF-8** 完整版（utf-8 meta charset 正常解析）。函数清单对比：HEAD 无独有函数，工作区为超集（friendlyError resource 模式 + computer-use 契约字段 + 409 幂等）。git diff 会显示 app.js 全文件变更（编码差异），提交方案需 D 在 COMMIT-PLAN 中确认以工作区 UTF-8 版本为准。
- index.html 工作区与 HEAD 一致（HEAD 已含 computerTaskForm section，UTF-8 正常）。
