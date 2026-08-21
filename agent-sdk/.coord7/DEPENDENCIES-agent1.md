# DEPENDENCIES-agent1.md — Agent 1 依赖需求与接线记录（R7）

> 我只写本文件。对其他 Agent/主控的依赖与接线说明在此留言。

## 对 Agent 3 的紧急请求（阻塞全量编译）

**`crates/owo-agent-core` 当前无法编译（13 个错误，全部来自你的 sandbox.rs 接线面），
已阻塞 server 全量编译与我的 scoped 门禁约 30 分钟。** 请优先修复并确认
`cargo check -p owo-agent-core` 通过后写 STATUS。

错误定位（`cargo check -p owo-agent-server --tests`）：

- `sandbox.rs:835/836` + `789`：`*mut c_void cannot be sent between threads safely`（×4）
- `sandbox.rs:1852/1847` + `1395/1610/1759/1791`：`mismatched types`（×5）+ `future cannot be sent`（×4）
- `sandbox.rs:1808`：`cannot borrow *child as mutable`（kill() 需要 &mut）
- `tools.rs:571/562/34`、`mcp.rs:92/101/111`：sandbox 接线后 `send` 约束连锁失败
  （`ShellExec`/MCP 子进程路径被 `SandboxManager` 泛型/引用要求波及）

我的 scoped 代码（auth_token/rate_limit/lib.rs 鉴权中间件/CLI audit/契约测试）已就绪，
一旦 core 可编译即可跑门禁。

## 对 Agent 4 的接线确认（已按留言完成）

- ✅ `event_stream::set_metrics_observer` → `observability_api::ingest_metrics_sample` 桥接已
  写入 lib.rs `build_router`（幂等注册）。
- ✅ `/metrics/slo` 已登记 OpenAPI（你的 router 自动挂载，无需主控加路由）。
- ✅ `/metrics/runtime`、`/metrics/slo`、`/auth/token` 已入 clients/ts 快照（175 路径）+
  schema.d.ts 重新生成 + typecheck 通过。
- 注意：你的 event_stream.rs 顶部注释“桥接在 lib.rs 接线”——若你改动了
  `MetricsSample`/`set_metrics_observer`/`ingest_metrics_sample` 签名，请在本文件留言。

## 对 Agent 2 的确认

- ✅ goal_api.rs 已按 RunnerConfig 新字段收尾修复（`use_worker_pool: false, worker_pool: None`）。
- ✅ worker_pool.rs 的 `BadLine.worker` clippy dead-code 由主控补齐（bad_lines 计数 + 字段）。
- ⚠️ 全量门禁实测：`pool_steps_execute_when_feature_flag_on` 等 3 个池测试在
  `cargo test --workspace` 与单测隔离下均**挂起 >4min**（submit 无预算 deadline 时永久 await，
  子进程不回报 result；子协议手工验证正常）。Agent 2 正在修复（23:30 有编辑），
  修好后请在本文件留言确认，主控复跑全量门禁。

## 主控收尾已完成的接线（R7）

- ✅ lib.rs：auth_token/rate_limit 中间件 + /auth/token 引导 + CORS 白名单 + 公开/保护面拆分；
  event_stream→observability 桥接 + slo report probe 注册；OpenAPI 补 /auth/token、/metrics/slo。
- ✅ route_contract_tests 8/8（新增 401/CORS/SSE 豁免/429+Retry-After+审计）。
- ✅ auth_token_tests 10/10、rate_limit_tests 21/21、CLI 10/10（audit_key 3 例）。
- ✅ clients/ts 快照 175 路径 + schema.d.ts 重新生成 + typecheck 通过。
- ✅ app.js token 引导 + 401 重试 + turn SSE 带 Authorization。
