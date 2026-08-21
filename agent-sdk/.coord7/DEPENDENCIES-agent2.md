# DEPENDENCIES-agent2.md — Agent 2 依赖与留言（R7）
> 多 Agent 并行协作：同一文件同一时间只允许一个 Agent 修改；需要他人文件时在此留言。

## ⚠️ 阻塞留言（给 Agent 3，请优先处理）
**`cargo check -p owo-agent-core` 当前被你的并行文件编译错误阻塞（11 处）**，我（Agent 2）的 scoped 门禁无法运行。错误清单（截至 22:05）：

1. `sandbox.rs:1848` / `mcp.rs`（经 `tools.rs:571`）：`WindowsProcess`/`JobGuard` 含 `*mut c_void` 不满足 `Send`/`Sync`（`JobGuard` 需包 `Send + Sync` 或改存储）。
2. `sandbox.rs:1755`：`OsChild::StdChild.child.id()` 返回 `u32`，函数签名要 `Option<u32>`（应为 `Some(child.id())`）。
3. `sandbox.rs:1787`：`read_pipe(out_handle)` 闭包 `*mut c_void` 跨线程（scoped thread 要求 Send）。
4. `sandbox.rs:1810`：`child.kill()` 需要 `&mut`（`&` 引用上调用）。
5. `tools.rs:524`：`deny_hit(command, ...)` 参数应为 `&command`。
6. `plugin.rs:447`：`sandbox_gate_for_mcp` 未定义（函数缺失/重命名）。
7. `mcp.rs:577` + `tools.rs:576/615/642`：`StdioTransport` 非 Send → `tokio::spawn`/`Tool` trait 失败。

修完后我的门禁将立即补跑。我的文件不依赖你模块的 API，只依赖 crate 能编译。

## 我对他人文件的依赖
| 依赖文件 | 归属 | 用途 | 状态 |
|---|---|---|---|
| `crates/owo-agent-core/src/audit.rs` | 共享（只读） | worker_pool 审计事件写入 `AuditLog` | ✅ 只读 |
| `crates/owo-agent-core/src/sandbox.rs` | Agent 3 | `IsolationMode::Sandbox` 仅定义字段不调用 | ✅ 无 API 依赖（仅编译期依赖 crate 整体可编译） |
| `crates/owo-agent-core/src/tools.rs` / `mcp.rs` / `plugin.rs` | Agent 3 | 无直接引用 | ✅ 无 API 依赖 |

## 留给主控/其他 Agent 的接线点
1. **`GoalRunner::RunnerConfig.use_worker_pool`（bool，默认 false）+ `worker_pool: Option<WorkerPool>`**：goal 步骤经子进程执行的唯一开关。server 层接入时构造 `WorkerPool` 并 spawn 对应 `WorkerSpec` 即可。
2. **`AgentBus::send_worker_event(from, to, &WorkerEvent)`**：worker 生命周期事件（Started/Crashed/Restarted/Fused/Stopped/BudgetAborted/Cancelled）进总线的统一入口；`from` 应为 pool/worker 标识，`to` 为监督者 agent（需先 `bus.register`）。
3. **`child::run_child_protocol`**：子进程协议入口，真实 worker 宿主（Agent 3 沙箱子进程）可直接复用，保证与 pool 协议兼容。
4. **新依赖**：无（未改 Cargo.toml；全部复用 tokio/futures/uuid/chrono/serde 既有依赖）。

## 我未触碰的文件（边界声明）
- `sandbox.rs`、`tools.rs`、`plugin.rs`、`mcp.rs`、`credentials.rs`、`audit_chain.rs`：只读，未修改（并行 Agent 3 独占）。
- `crates/owo-agent-server/**`、`Cargo.toml`：未修改。

## 请其他 Agent 注意
- Agent 1（主控收尾）：worker_pool 的 `WorkerEvent`/`WorkerEventKind` 已从 lib.rs 顶层导出，OpenAPI/面板如需展示 worker 事件可直接使用。
- 我的测试会用 PowerShell `Get-CimInstance` 做孤儿进程断言（Windows 专属），跨平台留待后续。
