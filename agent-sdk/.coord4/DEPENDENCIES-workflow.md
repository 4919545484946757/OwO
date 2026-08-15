# DEPENDENCIES-workflow.md — Agent C 依赖需求（Lane C）

> 只写本文件。对共享文件或其他 lane 的依赖需求在此留言；由主控统一处理。

## 依赖需求

- [Lane C] 2026-08-14 — 无需新增 Cargo 依赖（uuid/tokio/axum/serde_json/tempfile 均已在 owo-agent-server）。
- [Lane C] 2026-08-14 — 需要主控在 `crates/owo-agent-server/src/lib.rs` 做接线（收尾动作）：
  1. `mod workflow_api;`
  2. build_router 中合并：`merge(workflow_api::router(state.clone()))`（或等价写法）。
  3. route_contract_tests.rs 登记新路径与 sample_body；openapi_spec + clients/ts/openapi.json 登记 /workflow/* 路径。
  4. index.html + app.js 引入并挂载 desktop/web/panels/workflow.panel.js。
- [Lane C] 2026-08-14 — 对其他 lane 无文件依赖；与 Lane A/B/D 无路径冲突（前缀 /workflow 独立）。
- [Lane C] 2026-08-14 — 若主控希望在真实后端上运行工作流（非 MockBackend 文件沙箱），需要 core 后续提供真实 ActionBackend 接入（本轮禁止改 core，未实现）。

## 共享/注意事项

- workflow_api.rs 的模块内单例注册表按进程全局（OnceLock），测试使用唯一 run_id（时间戳+uuid），不跨测试污染。
- 工作流执行写 data_root/workflow-runs/<run_id>/，测试一律用 tempfile::tempdir() 的 data_root。
