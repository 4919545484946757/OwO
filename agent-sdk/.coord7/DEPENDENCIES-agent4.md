# DEPENDENCIES-agent4.md — Agent 4 依赖需求（R7）

> 只写本文件。对共享文件或其他 Agent 的依赖需求在此留言。

## 对主控的请求（R7 收尾接线，lib.rs 内两行 + 登记）

1. **lib.rs 接线（示例）**：
   ```rust
   pub mod slo; // 或 mod slo;
   // 初始化处（build_router 附近或 serve 启动路径）：
   crate::observability_api::register_slo_report_probe(std::sync::Arc::new(crate::slo::report_global));
   crate::event_stream::set_metrics_observer(Box::new(|sample| {
       crate::observability_api::ingest_metrics_sample(&sample.to_json());
   }));
   ```
   说明：observability_api.rs 与 event_stream.rs 均不引用 crate::，桥接由 lib.rs 完成；
   接线前 `/metrics/slo` 返回空报告、`/metrics/runtime` 的 SSE/事件字段为 0（均不 panic）。
2. **route_contract_tests**：`GET /metrics/slo`（GET 白名单，无 body、无 query）。
3. **OpenAPI 登记**：`/metrics/slo`；`/metrics/runtime` 响应补 `sse.lagged_total` 字段。

## 对其他 Agent 的依赖

- **Agent 1**：
  - `route_contract_tests.rs` 收尾时请一并修 2 个 clippy 错误（`await_holding_lock`：
    RATE_LIMIT_LOCK 跨 await；`manual_range_contains`：706 行 `ok_count >= 1 && <= 5`），
    并跑 `cargo fmt`（该文件当前有 fmt diff）。
  - lib.rs build_router 的 Router 类型接线问题在其文件内，非本 Agent 责任。
- **Agent 2 / Agent 3（core）**：我的 3 个测试 target 链接 owo-agent-server lib → 依赖
  owo-agent-core 可编译。两位编辑 worker_pool/fleet/goal/sandbox 期间的瞬态编译错误
  已解除（最终复核时 core 已稳定，全绿）。
- 与 Agent 3 无文件交集；observability_api.rs / event_stream.rs / slo.rs / soak.ps1 /
  observability.panel.js 为本 Agent 独占（Agent 1 只读不写）。

## 环境/编码

- 无新增环境变量；无凭据需求；soak 短模式默认 10 分钟（`-Seconds N` 冒烟），长模式 1h。
- 新文件 UTF-8：.rs 无 BOM；soak.ps1 带 UTF-8 BOM（PS5.1 无 BOM 按 ANSI 解析会乱码，
  与 gate.ps1 先例一致）。
