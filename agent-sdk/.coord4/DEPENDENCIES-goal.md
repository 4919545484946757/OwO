# DEPENDENCIES-goal.md — Lane D 依赖需求

> 只写本文件。对共享文件或其他 lane 的依赖需求在此留言；禁止直接改共享文件。

## 对主控的请求（lib.rs 接线，第四轮收尾清单第 1-4 项）

1. **模块挂载**：`crates/owo-agent-server/src/lib.rs` 加 `mod goal_api; mod sse;`，`build_router` 中合并：
   - `goal_api::router(state.clone())`
   - `sse::router(state.clone())`
   （两者均返回 `axum::Router`，与既有 `.merge()` 链兼容。）

2. **SSE 接入 cloud_task_submit**：`lib.rs::cloud_task_submit` 中把 `run_next` 的 ProgressSink 由 `NullSink` 换成
   `sse::sink(task_id.clone())`——`task_id` 为队列返回的本地任务 id（`cloud-0001` 等），前端面板用同一 id 订阅
   `/cloud/tasks/{id}/events`。

3. **openapi_spec 登记**（9 条）：`/goal`、`/goal/{id}`、`/goal/{id}/plan`、`/goal/{id}/run`、
   `/goal/{id}/status`、`/goal/{id}/abort`、`/goal/{id}/audit`、`/goal/{id}/runs`、`/cloud/tasks/{id}/events`。

4. **route_contract_tests**：
   - sample_body 新增：`/goal` → `{"objective":"t"}`；`/goal/{id}/plan` → `{"steps":[{"id":"a","worker":"echo"}]}`；
     `/goal/{id}/run` → `{}`；`/goal/{id}/abort` → `{}`。
   - resource_404_ok 白名单新增：`/goal/{id}`、`/goal/{id}/plan`、`/goal/{id}/status`、`/goal/{id}/audit`、
     `/goal/{id}/runs`（对未知 id 返回 404 属资源型）。
   - `/cloud/tasks/{id}/events` 为流式 SSE（连接不结束）：建议路由契约遍历对该路径特判（登记存在即可，不等待 body 完成）。

## 对其他 lane 的依赖

- 无。Lane D 只读 core 的 goal/plan/cloud_exec（未修改）；不依赖 A/B/C 的文件。
- 注意：`cargo clippy -p owo-agent-server --all-targets` 当前被 Lane B 的
  `plugin_market_api.rs`（未读字段）与 `plugin_market_api_tests.rs`（未用变量/多余 mut）阻塞，B lane 修复后主控可统一复跑。

## 已确认不需要的依赖

- 无新增 crate：axum（sse feature）、tokio（broadcast/mpsc/spawn）、tokio-stream（UnboundedReceiverStream）、
  serde/serde_json、uuid、tempfile（dev）均为既有依赖。
