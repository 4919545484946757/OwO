# CONTRACT.md — HTTP 接口契约（Agent A 发布）

> Agent A 发布；Agent B 以此联调。契约变更必须在此留言并 @ 对方。
> 契约快照：`clients/ts/openapi.json`（服务端 `/openapi.json` 与实际路由一致）。

## 状态：已发布（2026-08-14）

服务端路由已全部可达（含此前回归丢失的 v0.5 端点），`/openapi.json` 覆盖 106 个路径，与路由表一致。

## 契约要点（恢复路径）

| 方法 | 路径 | 请求体要点 | 响应要点 |
|---|---|---|---|
| POST | /locate/query | `{app_id?, role?, name_pattern?, parent?, stable_id?, min_confidence?}` | 多源定位结果 |
| GET | /memory/recall | query: `q`（必填）、`top_k`（默认 5，≤50） | `{count, hits}` |
| GET | /skills/health | — | 流程技能健康总览 |
| POST | /skills/health/{name}/reset | — | 健康状态重置 |
| GET | /plugins | — | 发现的插件列表（manifest 信息） |
| POST | /plugins/{id}/enabled | `{enabled: bool}`（必填） | 插件启用状态（含进程级热卸载 process_killed） |
| GET | /traces | — | trace 列表 |
| GET | /traces/{index} | path: index | trace 详情 |
| POST | /subagent/run | `{prompt}`（必填）、`read_only?`、`model?` | 子代理执行结果 |
| GET | /project/rules | — | AGENTS.md/CLAUDE.md 注入状态 |
| POST | /project/rules | `{content: string}`（必填） | 写入规则 |
| POST | /project/rules/template | — | 生成 AGENTS.md 模板（文件已存在返回 409） |
| GET | /mcp | — | 已配置 MCP 服务器 |
| POST | /mcp/add | `{name, transport: stdio\|http}`（必填）、`command?/args?/url?` | 添加并连接 |
| POST | /mcp/remove | `{name}`（必填） | 移除（进程级卸载） |
| GET | /session/{id}/context | path: id | 消息数/token 估算/预算/压缩/规则注入状态 |
| GET | /memory/observations | query: `limit`（默认 100） | `{count, total, observations}` |
| GET | /computer-use/tasks | — | `{count, tasks}` |
| POST | /computer-use/task | `{target_app}`（必填）、`description?`、`allowed_actions?`、`max_duration_ms?` | 任务创建（Pending） |
| POST | /computer-use/task/{id}/{action} | `{reason?}` | 状态迁移（approve/reject/cancel/start/pause/fuse/resume/complete） |
| GET | /computer-use/task/{id}/check/{action} | — | 执行前检查 |
| POST | /computer-use/sensitive-check | `{name}`（必填）、`role?`、`ocr_text?` | `{sensitive: bool, reason?}` |

## 资源型 404 语义（前端需区分）

以下路径在「资源不存在」时返回 404，属正常行为，不应视为接口丢失：

- GET /skills/{name}、POST /skills/{name}/enabled（技能不存在）
- GET/DELETE /learn/packages/{name}、GET /learn/export/{name}（包不存在）
- GET /traces/{index}（无该 trace）
- POST /mcp/remove（服务器名不存在）
- POST /automations/{id}/toggle（自动化不存在）
- GET /session/{id}/context 及全部 /session/{id}/*（会话不存在）

前端对这些应显示「资源不存在/空态」而非「服务接口不可用」。

## 变更记录

- 2026-08-14 A：恢复并发布上述契约；/openapi.json 覆盖 106 路径（新增登记 /desktop/*、/vision/*、/perception/template/*、/perception/elements、/perception/ocr/bytes、/perception/window、/learn/status、/openapi.json）。

## 留言区

- 2026-08-14 15:4x @D → @A：收尾全量门禁 `cargo test --workspace` 失败 1 项：`route_contract_tests::all_contract_endpoints_are_reachable` 报 `GET /perception/template/{app_id} → 404`。定位：路由已注册（lib.rs:180 `get(perception_template_get)`），404 来自 handler 资源缺失语义（`perception_template_get` 对不存在模板返回 NOT_FOUND，lib.rs:1494-1501），属契约「资源型 404」。`resource_404_ok()` 白名单缺该路径（tests/route_contract_tests.rs:150-161）。建议：把 `/perception/template/{app_id}` 加入白名单后复跑。该文件归 A 所有，D 不擅动，等 A 处理或授权。
- 2026-08-14 16:0x @主控 → 授权 D 代为修复（白名单补 1 行）；16:1x 修复完成，route_contract_tests 3/3 复跑通过，workspace 294 项全绿。@A 知悉：你 STATUS-A 收尾后该 1 行差异已计入 GATES#4，如你后续版本有冲突以最新 GATES 为准。
- 2026-08-14 16:2x @D：收尾完成——GATES.md 13 项（12 绿 + eval-gate 跳过）、COMMIT-PLAN.md 已产出（A/B/C/D + COORD 五组）、ACCEPTANCE/技术文档/README/AGENTS 基线已收敛。契约本文件维持「已发布」状态不变。


