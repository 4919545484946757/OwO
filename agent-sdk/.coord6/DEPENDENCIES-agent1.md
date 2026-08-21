# DEPENDENCIES-agent1.md — Agent 1 依赖需求与收尾接线记录（R6）

> 我只写本文件。对其他 Agent/主控的依赖与接线说明在此留言。

## 已按留言处理的接线点

| 来源 | 留言 | 处理 |
|---|---|---|
| Agent 4 DEPENDENCIES #1 | `mod event_stream; mod idempotency; mod error_codes;` 挂入 lib.rs | ✅ 已登记；event_stream 路由并入 build_router；idempotency/error_codes 为纯登记（无路由） |
| Agent 4 DEPENDENCIES #2 | observability 指标桥接（record_sse_connection 等）"可选" | ⏳ 下一轮做（需要 event_stream/observability 跨模块引用，会破坏两模块"不引用 crate::"的独立编译契约，故不在本轮硬接；已与 Agent 4 STATUS 风险一致） |
| Agent 4 DEPENDENCIES #3 | OpenAPI 登记 2 条 | ✅ /events/stream、/metrics/runtime 已入 openapi_spec + 快照 + schema.d.ts |
| Agent 4 DEPENDENCIES #4 | route_contract_tests SSE/资源型特判 | ✅ GET 无 body；/events/stream 直接 200（SSE 立即返回头）；/metrics/runtime 走普通 GET；测试每请求 60s 超时兜底 |
| Agent 3 STATUS 接线点 #1/#2 | core lib.rs `pub mod sandbox/credentials/audit_chain` + `pub use` | ✅ 已并入并导出（三文件自带 `#![allow(dead_code)]` 保留，无害） |
| Agent 3 STATUS 接线点 #3 | `owo-agent audit verify|export` 接 CLI main | ⏳ 下一轮（本轮按分工不接 main） |
| Agent 3 STATUS 接线点 #4 | 全量门禁跑 core 三件套 50 例 | ✅ 已随 workspace 全绿（sandbox 19 + credentials 11 + audit_chain 20） |
| Agent 2 STATUS | 无接线点（core 自包含） | ✅ goal_plan_tests 29 + fleet_tests 13 全绿 |

## 对其他 Agent 的依赖（并行期观察）

- Agent 2 编辑 fleet/blackboard/critic/goal 期间存在瞬态编译错误（E0405/E0425/E0428 窗口，约 20:24–20:26），阻塞过 server 侧测试编译；最终全量门禁时已稳定。
- Agent 4 的 event_stream.rs 在收尾接线前仅经 #[path] 测试编译，接线进 lib 后才暴露 lib-target dead_code 11 处 → 主控补模块级 allow（记录于 STATUS-agent1）。

## 收尾记录（供下一轮主控/Agent 参考）

- 本轮修复的编码/工程问题：lib.rs 2 处 GBK 双重编码、gate.ps1 缺 BOM（PS 5.1 解析失败）、route_contract 测试挂起（/eval/gate/run 真实 eval 分钟级）。
- 全量门禁命令（Windows，内存受限需 CARGO_BUILD_JOBS=2）见 STATUS-agent1.md。
