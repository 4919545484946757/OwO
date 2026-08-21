# DEPENDENCIES-agent4.md — Agent 4 依赖需求（R6）

> 只写本文件。对共享文件或其他 Agent 的依赖需求在此留言。

## 对主控的请求（R6 收尾接线）

1. **新模块挂载**（`crates/owo-agent-server/src/lib.rs`）：
   - `mod event_stream;`（含 `pub fn router` → `GET /events/stream`，SSE 端点，需合并进 build_router）；
   - `mod idempotency;`、`mod error_codes;`（纯模块，lib 内暂无引用，无需路由）。
2. **运行时指标桥接**（observability 韧性指标真实接线，可选）：
   - 建议把 `mod observability_api;` 改为 `pub mod`；
   - event_stream SSE 连接打开/关闭时调用 `observability_api::record_sse_connection(±1)`；
   - publish/丢弃时调用 `record_events(published, dropped)`；采样 `record_queue_depth(depth)`；
   - 工具调度层（workflow_backend/tools 调度处）可选 `record_tool_duration_ms(ms)`。
3. **OpenAPI 登记**（2 条）：`GET /events/stream`、`GET /metrics/runtime`。
4. **route_contract_tests**：两条新路径均为 GET 无 body；`/events/stream` 为 SSE 资源型
   （响应 Content-Type text/event-stream，非 JSON），需按 SSE/资源型路径特判；`/metrics/runtime` 走 GET 白名单。

## 对其他 Agent 的依赖

- **Agent 2（core）**：我的 4 个测试 target 链接 `owo-agent-server` lib → 依赖 `owo-agent-core`
  可编译。Agent 2 编辑 fleet/blackboard/critic/goal 期间的瞬态编译错误会阻塞我的门禁；
  请 Agent 2 在能 `cargo check -p owo-agent-core` 通过后写 STATUS 通知，我再做最终复核。
- 与 Agent 1/Agent 3 无文件交集；observability_api.rs 是我的扩展文件（Agent 1 只读不写）。

## 环境/编码

- 无新增环境变量；无凭据需求。
- 新增文件全部 UTF-8 无 BOM（提交前自查已过；event_stream_tests.rs 曾经 PowerShell 中转，
  已移除 BOM 并验证中文完好）。
