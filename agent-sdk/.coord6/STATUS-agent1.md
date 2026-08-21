# STATUS-agent1.md — Agent 1 状态（R6：R5 主控收尾 + 全量门禁恢复，阶段 0）

> 我只写本文件。任务来源：主控 R6 四 Agent 分工（2026-08-16）Agent 1 角色（阶段 0 收尾）。

## 交付文件（自有 + 收尾接线）

| 文件 | 状态 |
|---|---|
| `crates/owo-agent-server/src/lib.rs` | ✅ R5 五路由模块合并复核 + R6 接线：`mod event_stream`（合并 router）、`mod idempotency`、`mod error_codes`；OpenAPI 补 `/events/stream`、`/metrics/runtime`（173 路径）；修复 2 处历史 GBK 双重编码 mojibake |
| `crates/owo-agent-server/tests/route_contract_tests.rs` | ✅ 3/3：/team/export 等 404 白名单 +3；/eval/gate/run sample_body 改不存在套件（防真实凭据环境触发分钟级真实 eval）；permission sample_body 修正 `{"allow":true}`；每请求 60s 超时防挂起；新增"模块路由漏登记"扫描 + 快照⇄served spec 双向一致断言 |
| `crates/owo-agent-core/src/lib.rs`（收尾接线） | ✅ 按 Agent 3 留言补 `pub mod sandbox/credentials/audit_chain` + `pub use` 导出（audit_chain 9 项 / credentials 9 项 / sandbox 15 项 / scene 11 项） |
| `crates/owo-agent-server/src/event_stream.rs`（收尾接线，Agent 4 交付） | ✅ 主控接线后 lib 目标 dead_code 收敛：补模块级 `#![allow(dead_code)]`（与 team_api.rs 同款，测试面符号说明入注释） |
| `crates/owo-agent-server/src/idempotency.rs` / `error_codes.rs`（收尾接线，Agent 4 交付） | ✅ 按 Agent 4 留言登记 `mod` 入 lib.rs，同款模块级 allow |
| `clients/ts/openapi.json` | ✅ 快照 171→173 路径（/events/stream、/metrics/runtime） |
| `clients/ts/src/schema.d.ts` | ✅ `npm run generate:local` 重新生成（eventsStream/metricsRuntime 入型） |
| `desktop/web/index.html` / `app.js` | ✅ 复核 9 面板（notes/plugin-market/workflow/goal/team/eval/observability/memory/command）脚本引入 + PANEL_ORDER 注册 + mount |
| `scripts/gate.ps1`（收尾修复） | ✅ 补 UTF-8 BOM（PS 5.1 无 BOM 按 ANSI 解码导致解析失败，R5 交付脚本首次可运行） |
| `crates/owo-agent-cli/src/main.rs` | ✅ 只复核：`plugin catalog/check/verify/install` 可构建，无需修复 |
| `agent-sdk/ACCEPTANCE.md` | ✅ 新增"五十五、R5 四线收尾 + 阶段 0 门禁恢复" |
| `builGoal/综合技术开发文档-2026-08-16.md` | ✅ §2.2 现状更新、§7 阶段 0 标记完成、§8 M0 行状态 |
| `.coord6/STATUS-agent1.md` / `DEPENDENCIES-agent1.md` | ✅ |

## 门禁实测（最终全绿，收尾统一执行）

| 门禁 | 命令 | 结果 |
|---|---|---|
| fmt | `cargo fmt --all -- --check` | ✅ 0 差异 |
| clippy | `cargo clippy --workspace --all-targets -- -D warnings` | ✅ 0 警告 |
| workspace 测试 | `cargo test --workspace`（CARGO_BUILD_JOBS=2） | ✅ 691/691（core lib 265 + CLI 7 + server lib 3 + 集成 416） |
| build | `cargo build --workspace` | ✅ |
| 路由契约 | `cargo test -p owo-agent-server --test route_contract_tests` | ✅ 3/3（4.6s，含真实 HTTP smoke） |
| node | `node --check` app.js + 10 面板 | ✅ 0 错误 |
| gate.ps1 | `scripts/gate.ps1` | ✅ 4/4（fmt/clippy/server 测试/node） |
| TS SDK | `npm run typecheck` + `test:unit` | ✅ 0 错误 / 3/3 |
| serve 冒烟 | `owo-agent serve`（OWO_AGENT_DATA=临时目录，端口 4096） | ✅ /health 200、/openapi.json 173 路径、/metrics/runtime 200、/team/export 404（资源）、/team/audit、/command/audit、/eval/gate/reports、/memory/graph/entries、/workflow、/goal、/plugins/market、/intent/parse、/command/run 全 200；桌面页 + 9 面板脚本 200 |
| SSE 端到端 | POST /cloud/tasks（mock 传输）→ GET /cloud/tasks/cloud-0001/events | ✅ 历史重放 snapshotting→submitting→submitted→executing→fetching→succeeded 六帧；GET /events/stream content-type: text/event-stream |
| 编码 | 全量 .rs/.js/.json/.html UTF-8 严格解码扫描 | ✅ 无损坏（gate.ps1 含 BOM 为有意） |

## 测试数量

- 全量 691（较上轮 487 新增 204，含四线 R6 交付）；route_contract 3/3。

## 需主控接线点（已完成）

- event_stream 路由已并入 build_router；idempotency/error_codes 已登记；core 三件套已导出；
- 遗留（下一轮）：SSE→observability 指标桥接（record_sse_connection/record_events，Agent 4 留言"可选"）、`owo-agent audit verify|export` 接入 CLI main、OS 级沙箱/凭据库接入。

## 遗留风险

- 真实 eval（/eval/gate/run 带真实 OPENAI_API_KEY）未在收尾实测（外部验收项，C.4 保持"开放"）；契约测试已用不存在套件 body 规避挂起。
- Agent 2/3/4 并行期间存在瞬态编译错误窗口（黑板/critic/fleet 编辑期），最终全量门禁已稳定。
- 并行期间全量门禁只跑一次（收尾），四线交付以各自 STATUS 为准，主控复核以 workspace 全绿为准。
