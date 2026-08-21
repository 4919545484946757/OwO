# DEPENDENCIES-agent3.md — Agent 3 依赖/协作需求（R6）

> 只写本文件。对共享文件或其他 Agent 的依赖需求在此留言，由主控统一处理。

## 对主控（接线）的依赖

1. `crates/owo-agent-core/src/lib.rs`：
   - `pub mod sandbox;`、`pub mod credentials;`、`pub mod audit_chain;`
   - `pub use` 导出建议：sandbox（`SandboxPolicy/FileScope/NetworkPolicy/IsolationLevel/SandboxCommand/SandboxExecutor/SandboxProcess/SandboxProcessStatus/SandboxHandle/SandboxHealth/SandboxError/PlatformSupport/probe_platform_support/available_isolation/evaluate_capability/CapabilityEvaluation/SandboxManager/SandboxAuditLog/SandboxAuditEvent/SandboxEventKind/MockSandboxExecutor/inside_workspace`）、credentials（`ApiKeyRef/ProviderConfig/CredentialStore/MemoryCredentialStore/UnavailableStore/windows_credential_manager/CredentialResolver/CredentialError/scan_json_for_secrets`）、audit_chain（`AuditRecord/AuditExport/ChainedRecord/Anchor/AuditChain/AuditChainError/hmac_sha256/canonical/hex_encode/verify_export/export_to_file/load_export/verify_file/AuditCliCommand/AuditCliOutcome/run_audit_cli`）。
2. `owo-agent audit verify|export` 子命令：`crates/owo-agent-cli/src/main.rs` 接 `run_audit_cli`（本轮按指令不接）。
3. 全量门禁时跑 `cargo test -p owo-agent-core`（含 sandbox/credentials/audit_chain 50 例）。

## 对其他 Agent

- 无文件交集；不读取/修改其他 Agent 文件。
- 只读参考：`permissions.rs`（deny 优先模式）、`audit.rs`（AuditEntry）、`settings.rs`（BOM 剥离）、`gateway.rs`（OPENAI_API_KEY 约定）。

## 新增依赖

- **无**（HMAC-SHA256 用既有 `sha2` 就地实现；无需改 Cargo.toml）。

## 下一轮 OS 接入请求（先登记，不直接改）

- Windows Credential Manager：需 Cargo.toml 为 `windows` crate 增 `Win32_Security_Credentials` feature（Wave 2，经审批后加）。
- OS 级沙箱：`probe_platform_support()` 真实 AppContainer/Job Object 探测（windows-sys 既有 feature `Win32_System_Threading` 含 Job API，可复用；AppContainer 相关 API 可能需增 feature，到时一并登记）。

## 风险提示（供主控）

- 三个模块顶部 `#![allow(dead_code)]` 是 `#[path]` 独立编译的过渡；并入 lib.rs 后主控可移除并重跑 clippy。
- 审计链 key 建议托管于凭据库（X02 接入后），与导出文件分离存放；本轮测试全部用固定测试 key。
- `probe_platform_support` 当前对 Windows 也返回保守不可用（Wave 1 占位），接入 OS 实现后需补真实能力矩阵测试。
