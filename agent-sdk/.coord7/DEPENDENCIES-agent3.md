# DEPENDENCIES-agent3.md — Agent 3 依赖/协作需求（R7）

> 只写本文件。对共享文件或其他 Agent 的依赖需求在此留言，由主控统一处理。

## 对主控（接线）的依赖

1. `crates/owo-agent-core/src/lib.rs`：建议补充导出（现有 R6 导出之外新增的公开项）：
   - sandbox：`default_manager`、`ExecGuard`、`JobGuard`、`UnavailableExecutor`、`os_struct_layouts_match`、`SandboxWaitInfo`、`SandboxProcessInner`（可选）；
   - credentials：`WindowsCredentialManagerStore`、`CREDENTIAL_NAMESPACE`；
   - audit_chain：`AUDIT_KEY_STORE_KEY`。
2. 全量门禁时跑 `cargo test -p owo-agent-core`（等 Agent 2 修复 worker_pool_tests/goal_plan_tests 后）。

## 对其他 Agent

- 无文件交集；不读取/修改其他 Agent 文件。
- Agent 2 的 `worker_pool.rs` 若要用 OS 隔离：可复用 `SandboxPolicy.active_process_limit/mem_mb/cpu_ms` 与 `WindowsSandboxExecutor`（经 `default_manager()`）——预留的 `isolation` 接入点已具备。

## 新增依赖

- **无**（Job Object / 令牌 / AppContainer / Credential Manager 全部 raw FFI，零 Cargo.toml 改动）。

## 下一轮 OS 接入请求（先登记，不直接改）

- 若后续希望改用 `windows` crate 正式 API（替代 raw FFI）：需 Cargo.toml 为 `windows` crate 增 `Win32_Security_Credentials`、`Win32_System_JobObjects`、`Win32_Security` features；为 `windows-sys` 增 `Win32_Security_Credentials`、`Win32_System_JobObjects`。Wave 2 已用 raw FFI 完成同等功能，此项仅是可维护性优化，非必需。
- LowIL/AppContainer 执行路径的 stdout/stderr 采集（当前占位）与 AppContainer 网络 capability（SID 白名单）在 Wave 3 接入。

## 风险提示（供主控）

- `os_sandbox_integration_tests` 在无 Job Object 的环境（老系统/受限 CI）显式 SKIP（eprintln + return）；`OWO_FORCE_OS_TESTS=1` 可强制失败。
- `default_manager()` 全局单例在测试进程间共享；其审计事件会累积（`take_audit_events()` 可取走），不影响断言式测试（各测试用独立 `SandboxManager` 实例）。
- WCM 测试在凭据库被策略锁定的环境显式跳过（`wcm_store()` 门控）；本机（Windows 桌面会话）闭环通过。
- MCP/插件宿主 Job 进程上限放宽到 32（剪贴板插件需派生 powershell 子进程）；`run_command` 保持默认 1（防进程炸弹）。
