# STATUS-agent2.md — Agent 2 状态（多 Agent P0 收尾：fleet + critic/blackboard）
> 我只写本文件。任务来源：主控 R6 四 Agent 并行分工（2026-08-16）Agent 2 角色。

## 交付文件（自有）

| 文件 | 状态 |
|---|---|
| `crates/owo-agent-core/src/fleet.rs`（扩展） | ✅ fan_out 超时/取消传播/部分成功仲裁 + 等待图环检测与优先级仲裁 + 消息去重键 |
| `crates/owo-agent-core/src/critic.rs`（新建） | ✅ 只读评审原语：ReadOnlyGate / review_loop / 一致率报告 / ScriptedCritic |
| `crates/owo-agent-core/src/blackboard.rs`（新建） | ✅ 单写主 + Policy 门控 + 事件溯源 + 快照恢复（防篡改校验） |
| `crates/owo-agent-core/src/goal.rs`（扩展） | ✅ worker 层接入 critic（`_critic.rounds`）与 blackboard（`_bb.read/write`） |
| `crates/owo-agent-core/src/lib.rs`（导出） | ✅ 新增 `blackboard`/`critic` 模块与 fleet/编排原语顶层导出 |
| `crates/owo-agent-core/tests/fleet_tests.rs`（新建） | ✅ 13 例：总线背压/去重/id 单调/fan-out 超时/取消/部分成功/等待图 |
| `crates/owo-agent-core/tests/goal_plan_tests.rs`（扩展） | ✅ 新增 8 例 critic/blackboard worker 层契约测试 |
| `.coord6/STATUS-agent2.md`、`.coord6/DEPENDENCIES-agent2.md` | ✅ |

## 完成清单

### 1. fleet.rs 增强（P0 退出标准）
- **fan_out 超时**：`FanOutConfig.per_worker_timeout`（tokio timeout，到点取消 future）；超时 worker 标记 `TimedOut`。
- **取消传播**：`FanOutConfig.cancelled`（`Arc<AtomicBool>`）；置位后不再启动新 worker、在飞者 abort，未完成者 `Cancelled`；已成功结果保留。
- **部分成功仲裁**：`FanOutReport`（`succeeded()/failed()/retryable()/all_succeeded()`）；`FanOutOutcome.status`（Succeeded/Failed/TimedOut/Cancelled/Aborted，serde 兼容旧 JSON）；已成功结果保留 + `retryable()` 返回可单独重试子集。
- **等待图**：`WaitEdge`（waiter/waited/timeout）、`detect_wait_cycle`（handoff 环检测推广）、`WaitGraph`（周期扫描 + `resolve()` 按优先级取消低优先分支，并列取字典序最大保证确定性）。
- **背压保留**：原有 `DropMergeable` 策略（可合并进度事件丢弃 + 任务/结果/评审/拒绝关键事件保留）未改动，补总线级契约测试。
- **消息丢失/重复可检测**：`message_dedup_key`（correlation_id + kind + payload 摘要）+ `dedupe_messages`（保序去重）。

### 2. 编排原语（新建模块）
- **critic.rs**：`ReadOnlyGate`（Policy 门禁，`ensure_read_only()` 拒绝可写策略下的评审，写/执行/注入请求裁决 Deny）；`review_loop`（意见回流原作者，最多 N 轮，通过=approved 或 score≥min_score）；`ConsistencyReport`（评审 vs 人工一致率，P0 退出标准）。
- **blackboard.rs**：单写主（`NotWriter` 拒绝非写主写/删/转移，`transfer_writer` 仅当前写主可转移）；Policy 门控（只读策略下写/删拒绝，读恒允许）；事件溯源（seq 单调 append-only，快照恢复校验 seq 单调 + 与事件日志一致，防篡改）。

### 3. goal.rs worker 层接入
- `GoalRunner::attach_critic(CriticConfig)` + `attach_blackboard(Blackboard)`。
- 步骤 input 约定键（不侵入 plan.rs）：`"_critic":{"rounds":N}` → 输出经只读评审，意见注入 `_critic_feedback` 回流 worker 重跑；`"_bb":{"read":["k"],"write":"k2"}` → `{{bb:k}}` 占位符替换 + 成功后写回（写主=goal id）。
- 评审轮次计入步骤尝试（预算约束）；abort 标志在评审回流作者时检查。

## 门禁结果

| 门禁 | 状态 |
|---|---|
| `cargo fmt -p owo-agent-core -- --check` | ✅ 0 |
| `cargo clippy -p owo-agent-core --all-targets -- -D warnings` | ✅ 0 警告 |
| `cargo test -p owo-agent-core` | ✅ 全绿（含本 Agent 全部新增用例） |

## 测试数量（新增 ≥20 达标）

| 文件 | 新增用例 |
|---|---|
| `tests/fleet_tests.rs`（新建） | 13 |
| `tests/goal_plan_tests.rs`（扩展） | 8 |
| `src/critic.rs` 模块测试（新建） | 6 |
| `src/blackboard.rs` 模块测试（新建） | 7 |
| **合计** | **34**（全绿） |

## 需主控接线点
- 无（core crate 内自包含；`goal.rs` 编排原语经 `GoalRunner::attach_critic/attach_blackboard` 暴露，server 层接线由主控按 R6 协议统一处理）。
- 如需 `Agent::run_subagent` 真实接入 `Worker`（goal.rs 注释预留），由主控决定后续轮次。

## 遗留风险
- fan-out 取消为调度循环边界生效：取消时恰在飞的 worker 若先完成则保留成功结果（设计如此，部分成功语义），不保证硬中断在飞任务。
- `_critic`/`_bb` 约定键为 JSON 输入约定（`_` 前缀），文档位于 goal.rs 模块注释；若未来需要 schema 强类型化，可迁移到 StepSpec 新字段（涉及 plan.rs，需主控授权）。
- 黑板书事件日志为进程内 append-only；跨机 CRDT/持久化落盘留待后续轮次（单写主 + 快照已具备恢复能力）。
