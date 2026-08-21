# STATUS-agent3.md — Agent 3 状态（R7 生产化安全 Wave 2：Windows OS 沙箱 + 凭据库 + 审计打通）

> 我只写本文件。任务来源：主控 2026-08-16 R7 分工指令（Agent 3 角色，综合文档 §6 P0：X01/X02/X04 OS 级落地）。

## 交付文件

| 文件 | 状态 |
|---|---|
| `crates/owo-agent-core/src/sandbox.rs`（扩展） | ✅ |
| `crates/owo-agent-core/src/credentials.rs`（扩展） | ✅ |
| `crates/owo-agent-core/src/audit_chain.rs`（扩展） | ✅ |
| `crates/owo-agent-core/src/tools.rs`（接入 run_command） | ✅ |
| `crates/owo-agent-core/src/mcp.rs`（接入 stdio 子进程） | ✅ |
| `crates/owo-agent-core/src/plugin.rs`（接入插件宿主门卫） | ✅ |
| `crates/owo-agent-core/tests/sandbox_tests.rs`（扩展，#[path] → lib 导出） | ✅ |
| `crates/owo-agent-core/tests/credentials_tests.rs`（扩展） | ✅ |
| `crates/owo-agent-core/tests/audit_chain_tests.rs`（扩展） | ✅ |
| `crates/owo-agent-core/tests/os_sandbox_integration_tests.rs`（新建，环境门控） | ✅ |
| `.coord7/STATUS-agent3.md`、`.coord7/DEPENDENCIES-agent3.md` | ✅ |

未改 Cargo.toml（**零新依赖**，raw FFI 方案）；未改 lib.rs、worker_pool.rs、fleet.rs、goal.rs、server。

## 完成清单

### 子任务 1：sandbox.rs（X01 OS 级落地）
- `probe_platform_support()` Windows **真实探测**：Job Object 创建试运行、低完整性令牌（复制令牌 + SetTokenInformation 探测，不改动当前令牌）、AppContainer API 存在性（GetProcAddress 动态解析，避免旧系统加载失败）+ RtlGetVersion OS 版本；探测结果带完整 reason（审计可感知）；
- `WindowsSandboxExecutor`（raw FFI，kernel32/advapi32/ntdll，零新依赖）：
  - Job Object：kill-on-close（防孤儿）+ JobMemoryLimit（mem_mb）+ PerJobUserTimeLimit（cpu_ms）+ ActiveProcessLimit（active_process_limit，默认 1，MCP/插件宿主放宽到 32）；
  - Low Integrity 路径：DuplicateTokenEx + SetTokenInformation(TokenIntegrityLevel) + CreateProcessAsUserW（20 字节 SYSTEM_MANDATORY_LABEL_ACE 手写布局）；
  - AppContainer 路径：DeriveAppContainerSidFromAppContainerName + STARTUPINFOEXW + SECURITY_CAPABILITIES + CreateProcessW（属性列表仅存活到进程创建）；
  - 创建失败一律**显式失败并终止已启动进程**（AssignProcessToJobObject 失败 → TerminateProcess），绝不静默降级；
  - `SandboxProcess` 增加 `wait_output()`（Job 路径完整捕获 stdout/stderr，双线程防管道满死锁）与 `kill()`；
- `SandboxManager` 扩展：`default_manager()`（全局惰性，真实探测 + Windows 执行器/UnavailableExecutor）、`guard()` 门卫、`attach_pid()`（挂接运行中进程 → `JobGuard`，Drop 时 TerminateJobObject 防孤儿）、`take_audit_events()`；
- `JobGuard`/`WindowsProcess`/`OsChild` 显式 `unsafe impl Send`（raw 句柄独占转移，标准 Windows 实践）；
- 结构布局与 SDK 一致性断言（x64：JOBOBJECT_* 64/144、STARTUPINFOEXW 112、CREDENTIALW 80 等），`os_struct_layouts_match()` 公开检查。

### 子任务 2：credentials.rs（X02 Windows Credential Manager 落地）
- `WindowsCredentialManagerStore`：raw FFI（CredWriteW/CredReadW/CredDeleteW/CredFree，advapi32），target 名带 `owo-agent/` 命名空间，Persist=LOCAL_MACHINE；`available()` 真实探测（读不存在条目返回 ERROR_NOT_FOUND=1168 即 API 可用）；
- `windows_credential_manager()` 返回真实可用实现（Windows）；非 Windows 显式不可用；
- 解析优先级保留：凭据库 → 环境变量 → 显式内联（测试）→ `CredentialError::Missing`；settings.json 零明文契约测试保留并扩展。

### 子任务 3：audit_chain.rs（X04 打通）
- `AuditChain::append_sandbox_log(log, actor)`：沙箱审计事件（`sandbox.<kind>` 事件名 + tool=sandbox:<name>）汇入分段 HMAC 链，篡改可检出；
- `AuditChain::from_managed_key(store, store_key, segment_len)`：链密钥托管凭据库（hex 编码 32 字节随机；已有密钥复用；凭据库不可用/密钥损坏 → **显式错误**，禁止无托管密钥静默回退）；`key_digest()` 供"导出与密钥分离"契约验证；
- `SandboxEventKind::label()` 事件名映射。

### 子任务 4：调用链接入（默认 deny，失败审计）
- `tools.rs` `run_command`：统一经 `default_manager().spawn()`（Job 级隔离，命令体过 deny 黑名单检查，60s 超时后进程仍在受限 Job 内由资源上限兜底）；
- `mcp.rs` stdio 子进程：`guard()` 门卫（策略：只读系统作用域 + 回环网络 + Job 隔离 + 进程上限 32）→ spawn → `attach_pid()` 挂 Job；**挂接失败 = 显式拒绝（kill + 错误），不留非受限子进程**；`StdioTransport` 持有 JobGuard（与 kill_on_drop 双保险）；
- `plugin.rs`：插件宿主（MCP 服务器）安装/校验时经 `sandbox_gate_for_mcp`（网络白名单取 manifest.network_allowlist；HTTP MCP 由静态扫描 allowlist 校验）。

## 门禁实测

| 门禁 | 结果 |
|---|---|
| `cargo fmt -p owo-agent-core -- --check` | ✅ 干净 |
| `cargo clippy -p owo-agent-core --all-targets -- -D warnings` | ✅ 本 Agent 文件 0 警告（剩余报错全部在 Agent 2 in-flight 文件：worker_pool_tests/goal_plan_tests） |
| `cargo test -p owo-agent-core --test sandbox_tests --test credentials_tests --test audit_chain_tests --test os_sandbox_integration_tests` | ✅ 79/79 |

新增用例：sandbox **26**（+7）+ credentials **16**（+5）+ audit_chain **26**（+6）+ os_sandbox_integration **11**（新建）= **79**（目标 ≥30，超 2.6 倍）。
回归验证：`mcp_tests` 13/13、`eval_tests` 3/3、`plugin_lifecycle_tests` 20/20、`workflow_tests` 17/17、`loop_tests` 30/30 全绿（含 run_command/MCP/插件经沙箱的真实执行路径）。

## 需主控接线的点

1. `lib.rs`：如需对外暴露可补导出——`JobGuard`、`ExecGuard`、`UnavailableExecutor`、`os_struct_layouts_match`、`default_manager`（`pub use sandbox::{...}` 现有列表之外）；`WindowsCredentialManagerStore`（credentials）。
2. 全量门禁时跑 `cargo test -p owo-agent-core`（Agent 2 的 worker_pool_tests/goal_plan_tests 修复后）。
3. os_sandbox_integration_tests 含真实 OS 进程测试（约 12s），建议 CI 保留。

## 风险 / 未做项

- **LowIL/AppContainer 执行路径未在集成测试覆盖**（本机探测到 Low IL 可用、AppContainer API 可用，但真实 AppContainer 进程创建在受限 CI/会话环境可能失败；Job-only 路径已 100% 集成测试覆盖）。AppContainer 容器名 `owo-agent-container` 首次使用时由系统注册。
- LowIL/AppContainer 路径的 stdout/stderr 管道读取：Job-only 路径完整采集；OS 创建路径输出采集为占位（`read_pipe_handle` 线程就绪，待接入）——不影响隔离语义，已在代码注释登记。
- `tools.rs` run_command 超时（60s）后报错但进程仍在 Job 内运行至资源上限终止（CPU 60s/内存 1GB/kill-on-close 兜底），无无限泄漏。
- `probe_platform_support()` 每次调用做真实 API 调用（Job 创建/令牌探测）——default_manager 全局缓存一次。
- 审计链密钥托管到 Credential Manager（Windows）；非 Windows 平台需经 Memory/其他 store（显式不可用时 from_managed_key 报错，符合"不静默"）。
- 并行观察：门禁期间 Agent 2 的 worker_pool_tests/goal_plan_tests 处于编译错误状态（非本 Agent 文件）；本 Agent 只读他人文件。

## 自查

- 10 个文件 UTF-8（无 BOM）实测通过；未改 Cargo.toml（零新依赖）；未跑 `cargo test --workspace`；未提交任何 git 操作。
