# STATUS-agent4.md — Agent 4 状态（R6：可靠性与可观测性 Wave 1）

> 只写本文件。任务来源：主控 R6 四 Agent 分工指令（Agent 4）。
> 当前阶段：**开发完成，门禁复核中**（依赖 Agent 2 的 owo-agent-core 临时编译状态）。

## 完成清单

### 子任务 1：event_stream.rs（`crates/owo-agent-server/src/event_stream.rs`，新建）
- 单调 `seq`（AtomicU64，从 1 起全局递增）；`Last-Event-ID` 续传：`subscribe_after(last_event_id)` 重放
  历史中 `seq > last_event_id` 的事件后进入实时流（断线重连零丢失，历史窗口 4096）。
- 心跳：`heartbeat()` 发布可合并心跳事件；SSE 端点空闲 15s 发 keep-alive 注释帧。
- 有界订阅队列（默认 1024）：溢出时丢弃可合并事件（progress/heartbeat），保留关键事件
  （approval/circuit：先挤掉队内最旧可合并事件）；仍满则标记 lagged 断开慢消费者，发布方永不阻塞。
- 全局单例 `hub()` + `reset_hub_for_test`；路由 `GET /events/stream?last_event_id=`（SSE，供主控并入）。

### 子任务 2：idempotency.rs（`crates/owo-agent-server/src/idempotency.rs`，新建）
- 幂等键注册表 + 响应缓存（默认 10_000 条 / 24h TTL，插入满逐出最旧）。
- `key(correlation_id, operation)` 复合键；`execute` 在锁内完成 查→执行→缓存，并发重复提交
  executor 至多执行一次（零重复写）；`writes()/hits()` 计数供可观测性度量。
- 响应缓存保留 status/body/retry_after_ms，execute 时自动补记 correlation_id。

### 子任务 3：error_codes.rs（`crates/owo-agent-server/src/error_codes.rs`，新建）
- 分层错误码 `域/原因/可恢复性`（严格三段式解析，非法拒绝）。
- 已知注册表 12 项（gateway/permission/auth/validation/storage/tool/internal × 12 原因），
  HTTP 状态映射（429/503/504/403/401/400/409/404/502/500）+ `retry_after` 语义（显式标记覆盖注册表）。
- 未知原因兜底映射；`to_json` 统一错误响应体；`code()` 总构造器。

### 子任务 4：observability_api.rs 扩展（`crates/owo-agent-server/src/observability_api.rs`）
- 运行时指标注册表（静态 OnceLock）：工具调度耗时样本（上限 1000）、SSE 活跃/累计连接、
  队列深度、事件发布/丢弃计数；`record_*` pub 更新函数（供接线方/测试），`reset_runtime_metrics_for_test`。
- 新端点 `GET /metrics/runtime`：tool p95/p50、审批通过率/拦截率（审计面推导）、队列深度、
  SSE 连接、事件计数；空数据一律 null/0，不 panic。未改 overview/turns/tools/health 数据面。

### 子任务 5：observability.panel.js 扩展（`desktop/web/panels/observability.panel.js`）
- 新增“运行时韧性指标（Wave 1）”区块：工具调度 p95/p50、审批通过/拦截率、队列深度、
  SSE 活跃连接、事件流发布/丢弃（防御性取值，空数据 “—”）。

## 门禁实测（最终全绿）

| 门禁 | 命令 | 结果 |
|---|---|---|
| 事件流测试 | `cargo test -p owo-agent-server --test event_stream_tests` | ✅ 12/12 |
| 幂等测试 | `cargo test -p owo-agent-server --test idempotency_tests` | ✅ 7/7 |
| 错误码测试 | `cargo test -p owo-agent-server --test error_codes_tests` | ✅ 8/8 |
| 可观测性测试 | `cargo test -p owo-agent-server --test observability_tests` | ✅ 13/13（8 旧 + 5 新） |
| node | `node --check desktop/web/panels/observability.panel.js` | ✅ 0 错误 |
| fmt | 我的 8 个 .rs 文件 `rustfmt --check` | ✅ 干净（workspace 剩余差异在 Agent 1 的 route_contract_tests.rs） |
| clippy | `cargo clippy -p owo-agent-server --all-targets -- -D warnings` | ✅ 全绿 |
| 编码 | 11 个文件 UTF-8 无 BOM 校验 | ✅ 无 U+FFFD、中文完好 |

合计新增测试 **32 项**（12+7+8+5，≥24 达标）。

## 退出标准对照

- ✅ 新增用例 ≥24 且全绿（32 项）；
- ✅ 断线重连按 `Last-Event-ID` 零丢失（`subscribe_after_last_event_id_resumes_without_loss`）；
  慢消费者被断开而非拖垮调度器（`slow_consumer_lagged_and_publisher_never_blocks`，发布方 seq 持续推进）；
- ✅ 重复提交零重复写（`duplicate_submission_returns_cached_executor_runs_once` + 8 线程并发单次执行）；
  错误码与 HTTP 状态映射有契约测试（12 项注册表映射 + retry_after 语义）；
- ✅ 新指标可查询且空数据不 panic（`runtime_empty_data_no_panic`）；
- ✅ 未修改任何 R5 冻结文件（sse.rs 零 diff；lib.rs/route_contract_tests.rs 的 diff 均来自其他 Agent 的并行工作）。

## 需主控接线的点

1. `mod event_stream; mod idempotency; mod error_codes;` 挂入 lib.rs（三个新模块，均不引用 crate::）。
2. `build_router` 合并 `event_stream::router(state.clone())`。
3. event_stream SSE 接线：连接打开/关闭时调 `observability_api::record_sse_connection(±1)`、
   publish 时调 `record_events(published, dropped)`、`record_queue_depth`——需要把
   `mod observability_api` 改为 `pub mod` 或由主控在 lib.rs 内桥接调用。
4. OpenAPI 登记：`/events/stream`、`/metrics/runtime`。
5. route_contract_tests：两条新路径均 GET 无 body；`/events/stream` 为 SSE 资源型
   （Content-Type text/event-stream 特判），`/metrics/runtime` 走 GET 白名单。

## 遗留风险

- 队列深度/SSE 连接为“注册表 + 接线方调用”模型（本轮未真实埋点，符合“不新增核心埋点”约束）；
  未接线时 /metrics/runtime 的这两项恒为 0（不 panic）。
- 编译时序依赖 Agent 2 的 owo-agent-core 稳定（其编辑期间多次瞬态编译错误，最终复核时已稳定全绿）。
- 慢消费者断开后由 SSE handler 任务收尾 close（hub 内闭包已回收）；lagged 瞬时窗口内有计数。
