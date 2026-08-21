# DEPENDENCIES-agent2.md — Agent 2 依赖与留言
> 多 Agent 并行协作：同一文件同一时间只允许一个 Agent 修改；需要他人文件时在此留言。

## 我对他人文件的依赖
| 依赖文件 | 归属 | 用途 | 状态 |
|---|---|---|---|
| `crates/owo-agent-core/src/permissions.rs` | 共享（只读） | critic `ReadOnlyGate` 与 blackboard Policy 门控复用 `Policy::read_only`/`decision` | ✅ 只读使用，未修改 |
| `crates/owo-agent-core/src/plan.rs` | 共享（只读） | goal.rs 复用 `StepSpec`/`verify_output` 等 | ✅ 只读使用，**未修改**（严格按边界约定） |
| `crates/owo-agent-core/src/audit.rs` | 共享（只读） | `attach_audit` 既有接口未改动 | ✅ 只读 |

## 留给主控/其他 Agent 的接线点
1. **lib.rs 顶层导出已扩展**：`blackboard::*`、`critic::*`、fleet 新原语（`fan_out_cfg`/`FanOutReport`/`FanOutStatus`/`FanOutConfig`/`WaitGraph`/`WaitEdge`/`WaitResolution`/`detect_wait_cycle`/`arbitrate_wait_cycle`/`message_dedup_key`/`dedupe_messages`）——主控收尾时若需对 server 层路由暴露编排原语，直接 `use owo_agent_core::fleet::{...}` 即可。
2. **goal.rs 编排原语入口**：`GoalRunner::attach_critic(CriticConfig)`（async_trait Critic 由 server 层注入模型评审器）、`GoalRunner::attach_blackboard(Blackboard)`（构造时 `Blackboard::new(goal_id, policy)`，写主=goal id）。
3. **`fan_out` 旧签名保持不变**（`Vec<FanOutOutcome>`），新增强版本为 `fan_out_cfg`，不破坏任何既有调用。

## 我未触碰的文件（边界声明）
- `crates/owo-agent-core/src/plan.rs`：仅判断可复用已有 `detect_cycle` 语义，未做任何修改。
- `crates/owo-agent-server/*`、`crates/owo-agent-cli/*`、`desktop/*`：未修改。
- `Cargo.toml`：未新增任何依赖（黑板书时间戳用既有 `chrono`；去重键用 `std::hash`）。

## 请其他 Agent 注意
- Agent 1（主控收尾）：`cargo test --workspace` 全量跑时，goal_plan_tests 现为 29 例、fleet_tests 13 例，均为快速 tokio 测试（无网络）。
- 若 Agent 3 的安全硬化需复用 `Policy` 门控模式，blackboard/critic 的实现可作参考样例。
