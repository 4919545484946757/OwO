# STATUS-goal.md — Lane D 状态（Goal/Plan API + 云端 SSE + 编排面板）

> 我只写本文件。任务来源：主控第四轮分工指令（Agent D，Lane D）。

## 完成清单（全部完成并实测）

### Part 1 Goal/Plan HTTP API（`crates/owo-agent-server/src/goal_api.rs`，新建）
- 路由（全部在 `goal_api::router` 内注册，axum 0.8 花括号路径参数）：
  - `POST /goal {objective, budget?{max_steps,max_replans}}` → 201；`GET /goal` 列表；`GET /goal/{id}`（404 未知）
  - `POST /goal/{id}/plan {steps:[{id,worker,deps[],verify?,max_retries?,input?,parallel?}]}` → 201（`Plan::validate` 环检测 400 + `topological_waves` 预览）；`GET /goal/{id}/plan`
  - `POST /goal/{id}/run {config?{parallelism,allow_replan}}` → 202 {run_id}（`GoalRunner::new` + `attach_audit` + tokio::spawn 异步执行）
  - `GET /goal/{id}/status`（最新 `run-<id>.json` 的 GoalRunState 快照 + goal_status）；`POST /goal/{id}/abort`；`GET /goal/{id}/audit`（尾部 50 条）；`GET /goal/{id}/runs`
- 内置演示 worker：echo（回显 input.text）/ sleep（按 input.ms 毫秒睡眠）/ fail（按 input.text 注入失败，演示重试/replan）
- 存储：`data_root/goals/<goal_id>/{goal.json, plan.json, runs/run-<uuid>.json}`（复用 core persist/load）；运行注册表 `OnceLock<Mutex<HashMap<(goal_id, run_id), Arc<tokio::Mutex<GoalRunner>>>>>`（abort 用，std 锁内不跨 await）；审计按 data_root 键控的 `AuditLog`，写操作全部留痕
- 错误统一 `(StatusCode, Json({"error": ...}))`：未知 404、非法 400；未给 AppState 加字段

### Part 2 云端 SSE（`crates/owo-agent-server/src/sse.rs`，新建）
- `CloudSseHub`：task_id → `broadcast::Sender<String>` + 历史（≤512 条，订阅先重放再流式）；`hub()` 模块级 `OnceLock` 单例；`reset_hub_for_test`（仅首设生效）
- `SseHubSink`：`owo_agent_core::cloud_exec::ProgressSink` 适配器，`CloudProgress` 九变体 → JSON 帧（event/kind + 变体字段）；`sink(task_id)` 工厂
- 路由 `GET /cloud/tasks/{id}/events` → `Sse<UnboundedReceiverStream>`（text/event-stream，历史重放 + 实时；Lagged 跳过、Closed 结束）
- `pub fn router(state)` 可整体并入 build_router；`pub fn sse_frame_text` 供测试断言帧格式

### 面板（`desktop/web/panels/goal.panel.js`，新建）
- IIFE 注册 `window.OwoPanels.goal`；helpers 防御性降级（缺省自建 fetch/esc/friendlyError，baseUrl 缺省 `window.OwoPanels.baseUrl || "http://127.0.0.1:4098"`）
- 功能：目标列表/创建、步骤 JSON 编辑器、waves 预览、运行（parallelism=2/allow_replan）、状态轮询（步骤状态表 + 徽章）、abort、审计尾部、云端进度区（EventSource 订阅 `/cloud/tasks/{id}/events` 日志）
- 样式类全部 `owo-goal-` 前缀 + mount 注入 `<style>`；渲染全部经 `H.esc`（无 innerHTML 注入原始数据）

## 门禁实测（全部通过）

| 门禁 | 命令 | 结果 |
|---|---|---|
| goal API 测试 | `cargo test -p owo-agent-server --test goal_api_tests` | ✅ 12/12（创建/列表/404/400、环检测 400、waves 预览、echo+sleep 全成功、fail 触发 replan 且审计含 replan、abort→Aborted、恢复一致性、未知 goal 全子资源 404、审计尾部、plan 缺失 404） |
| SSE 测试 | `cargo test -p owo-agent-server --test cloud_sse_tests` | ✅ 6/6（hub 历史重放+实时、sink 帧序列与 CollectingSink 一致、帧 event/kind 字段、端点 text/event-stream、历史重放、task_id 隔离） |
| fmt | `rustfmt --check`（我的 5 个文件） | ✅ 干净（`cargo fmt --all --check` 剩余差异仅 B lane plugin_market_api.rs） |
| clippy | `cargo clippy -p owo-agent-server --lib --tests -- -D warnings` | ✅ 我的文件 0 警告（剩余 error 全部在 B lane plugin_market_api*，非本 lane） |
| node | `node --check desktop/web/panels/goal.panel.js` | ✅ 0 错误 |

合计新增测试 **18 项**（≥14 达标）。

## 需要主控接线的点

1. `lib.rs`：`mod goal_api; mod sse;` + `build_router` 合并 `goal_api::router(state.clone())` 与 `sse::router(state.clone())`。
2. `lib.rs::cloud_task_submit`：把 `run_next(&sink)` 的 sink 换成 `sse::sink(task_id.clone())`（当前为 NullSink），即可让 `/cloud/tasks/{id}/events` 收到真实进度。
3. `openapi_spec` 登记：`/goal`、`/goal/{id}`、`/goal/{id}/plan`、`/goal/{id}/run`、`/goal/{id}/status`、`/goal/{id}/abort`、`/goal/{id}/audit`、`/goal/{id}/runs`、`/cloud/tasks/{id}/events`。
4. `route_contract_tests.rs`：新 POST 路由 sample_body（/goal 等）、resource_404_ok 白名单（GET /goal/{id}、/goal/{id}/plan、/goal/{id}/status、/goal/{id}/audit、/goal/{id}/runs 对未知 id 404 属资源型）。
5. `index.html`/`app.js`：引入 `panels/goal.panel.js` 并 mount（导航项"编排"）。
6. SSE 端点 `/cloud/tasks/{id}/events` 会长期挂起连接——route_contract 全路径遍历测试如纳入需在资源型/流式白名单处理（建议仅登记不请求，或按 events 端点特判）。

## 风险 / 未做项

- `reset_hub_for_test` 因 OnceLock 仅首设生效，跨测试隔离靠 task_id 唯一（已测）；若主控需要真正重置需换 RwLock 实现。
- 运行态注册表在 server 进程生命周期内保留已完成 runner 句柄直到 run 结束移除（已移除）；abort 后 run 任务自行退出。
- 未做：goal 运行进度 SSE（Part 2 仅 cloud 进度）；run 级日志流式推送（面板用轮询）。
- 面板 EventSource 依赖后端 CORS 允许（build_router 已 permissive CORS，主控接线时无需额外处理）。
