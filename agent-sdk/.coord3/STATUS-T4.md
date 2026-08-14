# STATUS-T4.md — Agent T4 状态（§12 Goal/Plan 多 Agent 编排）

> 我只写本文件。任务来源：主控 2026-08-14 第三轮四条主线分工指令（T4）。

## 认领

- 时间：2026-08-14
- 任务：Goal/Plan 编排 core 库 v1——目标→计划→并行 worker→验证→仲裁→恢复（技术文档 §12 底座 / 续写 §15）。P1 全部完成。
- 白名单：`core/src/goal.rs`、`core/src/plan.rs`（新建）、`core/tests/goal_plan_tests.rs`（新建）。
- 只读引用：agent.rs（run_subagent 签名勘察）/ subagent.rs / audit.rs（AuditLog::record 复用）——未修改。
- 禁止项遵守：未改 Cargo.toml/lock、server、CLI、desktop、他人文件；未 commit。
- 例外记录（主控授权）：① lib.rs 追加 `pub mod goal; pub mod plan;` 两行（主控未及时处理，授权自加）；② 代主控在 owo-agent-core/Cargo.toml 加 T2 请求的 `ed25519-dalek = "2"` 与 `sha2 = "0.10"`（T2 的 plugin.rs 已使用但依赖未加，阻塞全 workspace 编译，主控授权代加）。

## P1 里程碑完成情况（全部完成）

1. **Goal** ✅ `Goal`（objective/状态/预算/验收条件）+ 状态机 Pending→Planning→Running→Verifying→Succeeded/Failed/Aborted（`transition` + `is_terminal`）；`GoalBudget`（步骤数/每步重试/全局重试/replan 次数/时长）。
2. **Plan DAG** ✅ `Plan` + `StepSpec`（依赖边/可并行标记/worker 名/输入规格/验证断言/重试策略）；`validate()`（id 唯一/依赖存在/`find_cycle` DFS 三色环检测）；`topological_waves()` 分层拓扑；`persist`/`load`（`<dir>/<plan_id>.json`）。
3. **调度器** ✅ `GoalRunner::run()`：wave 就绪集 + `JoinSet` 真并发 + max_parallel 限流；`Worker` trait + `WorkerRegistry`（按名派发；测试 MockWorker，真实接入 Agent::run_subagent 由主控后续做）。
4. **验证与仲裁** ✅ 步骤级 `verify_output` 断言（Contains/Equals/NonEmpty/Custom）；步内重试（预算内）；失败→replan（只重置失败步骤及其未完成后代，已 Succeeded 保留不重跑）；目标级 acceptance 汇总断言；全程审计（顶层 events + 可选注入 AuditLog）。
5. **恢复** ✅ `GoalRunState` 整体持久化（`<dir>/<run_id>.json`，含 goal/plan/records/计数器）；`from_state` 恢复后已完成步骤不重跑（幂等测试实证）；`abort` 立即停止、未完成步骤标记 Aborted 保留现场；步骤数/全局重试/时长预算熔断（Budget 错误直接失败不走 replan）。

## 门禁实测（时间戳 2026-08-14 收尾）

| 门禁 | 命令 | 结果 |
|---|---|---|
| T4 集成契约 | `cargo test -p owo-agent-core --test goal_plan_tests` | ✅ 21/21 |
| goal 单测 | `cargo test -p owo-agent-core --lib goal::` | ✅ 2/2 |
| plan 单测 | `cargo test -p owo-agent-core --lib plan::` | ✅ 9/9 |
| clippy | `cargo clippy -p owo-agent-core --lib -- -D warnings` + `--test goal_plan_tests` | ✅ 0 警告 |
| fmt | `rustfmt --check`（我的 3 个文件）| ✅ 干净（workspace fmt 差异仅 T2 的 plugin_lifecycle_tests.rs，非本域） |
| workspace | `cargo test --workspace` | ⚠️ 被 T3 的 workflow_tests 11 项失败阻塞（T3 域，非 T4 问题；T4 相关 32 项全绿） |

## 产出文件

- `core/src/plan.rs`（新建）：Plan/StepSpec/StepStatus/VerificationSpec/verify_output/拓扑/环检测/持久化 + 9 单测。
- `core/src/goal.rs`（新建）：Goal/GoalStatus/GoalBudget/Worker/WorkerRegistry/StepRecord/GoalRunState/GoalRunner/run_step_attempts + 2 单测。
- `core/tests/goal_plan_tests.rs`（新建）：21 项集成契约（三步骤并行+汇合验证、并行度峰值、重试成功/耗尽、验证失败 replan 不重跑成功步骤、预算熔断×2、abort 保留现场、恢复幂等不重跑、审计事件+注入、验收通过/失败、链式串行、持久化往返、worker 派发、未知 worker 报错、状态机迁移）。
- `core/src/lib.rs`：+2 行 `pub mod goal; pub mod plan;`（主控授权）。
- `owo-agent-core/Cargo.toml`：+ed25519-dalek/sha2（代主控处理 T2 请求，主控授权）。

## 公开 API 导出清单（供主控 lib.rs 统一整理）

- `pub mod plan;` → `Plan`、`StepSpec`、`StepStatus`、`VerificationSpec`、`verify_output`
- `pub mod goal;` → `Goal`、`GoalStatus`、`GoalBudget`、`Worker`、`WorkerRegistry`、`StepRecord`、`GoalRunState`、`GoalRunner`、`RunnerConfig`
- 建议 pub use 追加（可选）：`pub use goal::{Goal, GoalStatus, GoalBudget, Worker, WorkerRegistry, GoalRunState, GoalRunner, RunnerConfig};`、`pub use plan::{Plan, StepSpec, StepStatus, VerificationSpec, verify_output};`

## 遗留问题

1. 真实 worker 接线：`Agent::run_subagent` 尚未包成 Worker（主控后续做，trait 已就绪）。
2. 进度事件流（steps 状态变化通知）未做（P2 项；当前靠轮询 state/records 或审计事件）。
3. workspace 全量门禁被 T3 的 workflow_tests 11 项失败阻塞（rollback/loop/precondition/subflow 相关），需 @T3 修复后主控统一复核。
4. T2 的 plugin_lifecycle_tests.rs 有 fmt 差异（非本域，需 T2 自修）。
5. 预算计数为每尝试 +1（重试计入），文档口径如与产品期望不同可在接线时调整。
