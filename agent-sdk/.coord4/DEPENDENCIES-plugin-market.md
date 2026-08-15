# DEPENDENCIES-plugin-market.md — Lane B 依赖/协作需求

> 只写本文件。对共享文件或其他 lane 的依赖需求。

## 对主控（接线）的依赖

1. `crates/owo-agent-server/src/lib.rs`：加 `mod plugin_market_api;`，build_router 中合并 `plugin_market_api::router(state.clone())`（与 notes/workflow/goal/sse 路由并列）。
2. `tests/route_contract_tests.rs`：为新增 POST 路由补 sample_body：
   - `/plugins/market/seed` → `{"entries":[]}`
   - `/plugins/market/verify`、`/plugins/market/install` → `{"dir":"."}`
   - `/plugins/market/update` → `{"id":"x","dir":"."}`
   - `/plugins/market/uninstall` → `{"id":"x"}`
3. `clients/ts/openapi.json`（主控收尾重新抓取）与 openapi_spec 登记：/plugins/market、/plugins/market/seed、/plugins/market/versions、/plugins/market/verify、/plugins/market/install、/plugins/market/update、/plugins/market/uninstall、/plugins/market/scan、/plugins/market/audit。
4. `desktop/web/index.html` + `app.js`：引入 `panels/plugin-market.panel.js` 并挂载（nav 项 + OwoPanels["plugin-market"].mount）。

## 对其他 lane

- 无（Lane B 与 A/C/D 无文件交集；core::plugin 只读，未修改）。

## 新增依赖

- 无（复用 core 既有依赖；测试签名用固定常量，不引入 ed25519-dalek 到 server）。

## 风险提示（供主控）

- 环境变量 `OWO_PLUGIN_REQUIRE_SIGNATURE` 进程级：测试串行化处理完成；接线后若 HTTP 面需要动态切换签名策略，建议后续在 settings 中持久化（本轮未做，遵循"不新增 AppState 字段"约束）。
