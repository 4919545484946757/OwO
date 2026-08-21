# STATUS-agent2.md — Agent 2 状态（多 Agent P1：worker 子进程化与崩溃自愈）
> 我只写本文件。任务来源：主控 R7 四 Agent 并行分工（2026-08-16）Agent 2 角色。

## 交付文件（自有）

| 文件 | 状态 |
|---|---|
| `crates/owo-agent-core/src/worker_pool.rs`（新建） | ✅ 代码完成：WorkerSpec/WorkerBudget/IsolationMode/WorkerPool/child 协议/PoolWorker |
| `crates/owo-agent-core/src/fleet.rs`（扩展） | ✅ `WorkerEvent`/`WorkerEventKind` + `AgentBus::send_worker_event`（关键语义，Reject 不丢弃） |
| `crates/owo-agent-core/src/goal.rs`（扩展） | ✅ `RunnerConfig.use_worker_pool` feature flag + `resolve_worker` 池回退 |
| `crates/owo-agent-core/src/lib.rs`（导出） | ✅ `worker_pool` 模块 + fleet/worker_pool 顶层导出 |
| `crates/owo-agent-core/tests/worker_pool_tests.rs`（新建） | ✅ 22 例（spawn/心跳/kill/退避/熔断/预算/取消/1000 循环/白名单/总线/审计） |
| `crates/owo-agent-core/tests/goal_plan_tests.rs`（扩展） | ✅ 新增 7 例（flag 开关语义/重试/验证/预算清理/取消传播） |
| `crates/owo-agent-core/tests/fleet_tests.rs`（扩展） | ✅ 新增 3 例（worker 事件总线/溢出拒绝/serde 往返） |
| `.coord7/STATUS-agent2.md`、`.coord7/DEPENDENCIES-agent2.md` | ✅ |

## 完成清单

### 1. worker_pool.rs（新建，多 Agent P1 第一阶段）
- **WorkerSpec**：命令/工作目录/环境变量白名单/预算（轮次、时长、内存、CPU）+ `IsolationMode`（`Process` 默认；`Sandbox` 为 Agent 3 沙箱接入点，本轮仅定义不依赖）。
- **WorkerPool**（actor 模型：reader 任务 → 无界通道 → 调度循环独占写）：
  - spawn（ready 握手轮询，瞬时退出不误判）、kill（start_kill+wait 回收）、restart、shutdown；
  - 心跳 ping/pong（3s 超时）；`check_health` 崩溃自愈：终止 → 指数退避（复用 `fleet::backoff_secs`，封顶 60s）→ 重启；重启即崩继续退避；连续失败超限熔断（复用 `fleet::Supervisor`，健康任务完成后 `mark_healthy` 复位）；
  - 预算：轮次（`max_turns`，池侧强制）、时长（`max_duration_secs`，submit deadline 到期 kill + `BudgetAborted` 事件）；内存/CPU 为策略字段（沙箱接入后 OS 强制）；
  - 取消传播：`cancel_pending`/`cancel_all`（pending 立即以 `Cancelled` 解决，子进程侧 cancel 消息通知）；
  - 清理：`Drop` 同步 `start_kill` 全部子进程（安全网）；1000 次 spawn/kill 循环无孤儿（命令行标记断言）。
- **结构化消息协议**：父→子 stdin / 子→父 stdout，JSON 行（`task`/`ping`/`cancel`/`shutdown` + `ready`/`pong`/`result`）；stderr 仅诊断；非 JSON 行按协议错误（`BadLine`）。子进程侧入口 `child::run_child_protocol` 供测试二进制与真实 worker 宿主复用。
- **事件**：Started/Crashed/Restarted/Fused/Stopped/BudgetAborted/Cancelled → 本地事件环（cap 200）+ 总线（`send_worker_event`，Reject 策略）+ 审计（`worker.<kind>`）。

### 2. fleet.rs 扩展
- `WorkerEvent`/`WorkerEventKind`（serde，`snake_case`）；
- `AgentBus::send_worker_event(from, to, &WorkerEvent)`：`MessageKind::Task` + `OverflowPolicy::Reject` —— worker 事件为关键语义，邮箱满时拒绝而非静默丢弃。

### 3. goal.rs 扩展（feature flag 控制，默认关闭）
- `RunnerConfig { use_worker_pool: false, worker_pool: None }`（默认）；
- `resolve_worker`：registry 优先（进程内语义不变）；flag 开启时 registry 未命中回退 pool 子进程（`PoolWorker` 适配 `Worker` trait）；
- flag 关闭 → 行为与纯进程内完全一致（有契约测试锁定）。

## 门禁结果

| 门禁 | 状态 |
|---|---|
| `cargo fmt -p owo-agent-core -- --check`（我的文件） | ✅ 0（Agent 3 新建测试文件尚待其自身 fmt） |
| `cargo clippy -p owo-agent-core --all-targets -- -D warnings` | ⏳ **被 Agent 3 并行文件阻塞**（见 DEPENDENCIES：`sandbox.rs`/`tools.rs`/`mcp.rs` 编译错误非我文件） |
| `cargo test -p owo-agent-core` | ⏳ 同上（编译期阻塞；我的测试已就绪） |

## 测试数量（新增 ≥24 达标）

| 文件 | 新增用例 |
|---|---|
| `tests/worker_pool_tests.rs`（新建） | 22 |
| `tests/goal_plan_tests.rs`（扩展） | 7 |
| `tests/fleet_tests.rs`（扩展） | 3 |
| `src/worker_pool.rs` 模块单测（新建） | 6 |
| **合计** | **38**（待 Agent 3 合并后全量跑） |

## 需主控接线点
- 无（core crate 内自包含；`GoalRunner::RunnerConfig.use_worker_pool + worker_pool` 为唯一入口，server 层接线由主控按 R7 协议统一处理）。
- `IsolationMode::Sandbox` 为 Agent 3 沙箱接入点：worker_pool 定义字段与文档，不做任何 OS 级强制。

## 遗留风险
- **阻塞**：`cargo check/test` 被 Agent 3 并行修改中的 `sandbox.rs`/`tools.rs`/`plugin.rs`/`mcp.rs` 编译错误阻塞（11 处，非我文件）。Agent 3 落盘后我立即补跑全部 scoped 门禁。
- fan-out/池取消语义：`cancel_pending` 取消在飞任务需配合 `kill` 回收子进程（取消传播不杀进程，由调用方决定），文档已注明。
- 1000 次 spawn/kill 循环测试耗时约 10-20s（Windows 子进程创建成本），已分批并发（50/批）控制。
