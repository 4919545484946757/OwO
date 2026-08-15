# STATUS-workflow.md — Agent C 状态（Lane C：工作流 HTTP API + 工作流面板）

> 我只写本文件。任务来源：主控 2026-08-14 第四轮分工指令（Lane C）。
> 白名单（只能新建）：`crates/owo-agent-server/src/workflow_api.rs`、`crates/owo-agent-server/tests/workflow_api_tests.rs`、`desktop/web/panels/workflow.panel.js`、`.coord4/STATUS-workflow.md`、`.coord4/DEPENDENCIES-workflow.md`。

## 认领

- 2026-08-14：把上一轮 T3 交付的 .owflow 工作流引擎接到 HTTP API（/workflow/*）与桌面面板。
- 协议遵守：只新建文件；不改 lib.rs/route_contract_tests/openapi.json/desktop/web 既有文件/core/Cargo.toml；测试仅 tempdir；server 模块内不用 crate::/super::；AppState 全限定名；错误统一 (StatusCode, Json({error})); 无新字段（模块内 OnceLock 注册表）；写操作审计。

## 完成清单（2026-08-14）

1. `crates/owo-agent-server/src/workflow_api.rs`（新建）：
   - `pub fn router(state: Arc<owo_agent_server::AppState>) -> axum::Router`（/workflow 前缀全部路由）。
   - GET /workflow：递归（深度上限 3）发现 state.workspace 下 *.owflow，返回 [{name, path}]。
   - GET /workflow/{name}：加载 + validate_definition，返回 {definition, valid, issues}；未知 → 404。
   - POST /workflow/validate：内联定义校验 {valid, issues}；非法 JSON → 400。
   - POST /workflow/{name}/run {ctx?} → 201 {run_id}：加载定义 → 建 run（data_root/workflow-runs/<run_id>/ 为 MockBackend root）→ 注册表登记 → tokio::spawn 异步 run（run 前 20ms 窗口供 abort；检查 abort_requested 先 abort() 再 run()）；结果落盘 outcome.json（手动 json! 拼 state/steps/rollback_to，WorkflowOutcome 无 Serialize）+ audit.json。
   - GET /workflow/{name}/runs：该流程的 run 列表（注册表 + 落盘目录扫描）。
   - GET /workflow/run/{run_id}：快照 {run_id, name, state, steps, rollback_to, created_at, outcome}；未知 run → 404。
   - POST /workflow/run/{run_id}/abort：置 abort_requested + try_lock 立即 abort；未知 run → 404。
   - GET /workflow/run/{run_id}/audit：审计尾部（≤50 条）。
   - 运行注册表：模块内 OnceLock<Arc<std::sync::Mutex<HashMap<String, Arc<RunEntry>>>>>。
2. `crates/owo-agent-server/tests/workflow_api_tests.rs`（新建，18 用例全绿）：
   - discover 发现 .owflow、ignore 非 owflow 文件
   - 加载+校验通过 / 非法 DSL → valid=false issues 非空 / 未知流程 404
   - 内联 validate 通过 / 非法 400
   - 成功运行 → Succeeded 且步骤日志非空 + outcome.json 落盘
   - 前置条件失败 → Failed + rollback_to=最近检查点
   - 回滚语义：检查点后写入文件被回滚（经 API 落盘断言）
   - abort → Aborted（20ms 窗口内 abort）
   - runs 列表 / run 快照 / 未知 run 404
   - audit 尾部非空
   - 超时轮询到终态（tokio::time::timeout 包装）
3. `desktop/web/panels/workflow.panel.js`（新建，node --check 通过）：IIFE 注册 window.OwoPanels.workflow；流程列表/定义预览/validate/ctx+运行/runs+步骤时间线/abort/audit；helpers 防御性降级；样式 owo-workflow- 前缀。
4. `.coord4/DEPENDENCIES-workflow.md`（新建）。

## 门禁结果（2026-08-14 实测）

| 门禁 | 结果 |
|---|---|
| cargo fmt --all -- --check | ✅ 我的文件干净（workflow_api.rs / workflow_api_tests.rs / workflow.panel.js 均无 diff；全 workspace 其他 lane 的 sse/notes/goal/plugin 文件差异不归本 lane） |
| cargo clippy -p owo-agent-server --all-targets -- -D warnings | ✅ 我的文件 0 警告（全量 clippy 被其他 lane 的 goal_api_tests/notes_api_tests/plugin_market_api_tests 半成品阻塞，与本 lane 无关） |
| cargo test -p owo-agent-server --test workflow_api_tests | ✅ 18/18 全绿（目标 ≥10） |
| node --check desktop/web/panels/workflow.panel.js | ✅ 0 错误 |

## 需主控接线的点

1. `lib.rs`：`mod workflow_api;` + build_router 合并 `workflow_api::router(state)`。
2. `route_contract_tests.rs`：登记 /workflow 系列路径（sample_body：POST run → `{}`、POST validate → 最小合法定义、POST abort → 无 body）。
3. `openapi_spec` + `clients/ts/openapi.json`：登记 /workflow/* 路径。
4. `index.html` + `app.js`：引入 workflow.panel.js 并挂载。
5. 设计说明：WorkflowEngine::run 不可中断（core 无外部取消），abort 依赖 run 前 20ms 窗口 + abort_requested 标志（测试已验证稳定）；ctx 参数 v1 仅记录不注入（引擎无 set_ctx 接口，禁止改 core）；执行后端为 MockBackend 文件沙箱（data_root/workflow-runs/<run_id>/）。

## 风险

- run 期间 abort 的强中止（运行时熔断）需 core 引擎协作取消（后续增强）。
- GET /workflow 发现深度上限 3，避免遍历过大 workspace。
- 真实桌面/网络动作需主控后续接真实 ActionBackend（core trait 已定义）。

