# STATUS-agent3.md — Agent 3 状态（R6 生产化安全硬化 Wave 1：sandbox / credentials / audit chain）

> 我只写本文件。任务来源：主控 2026-08-16 R6 分工指令（Agent 3 角色，综合文档 §6 P0：X01/X02/X04）。

## 交付文件

| 文件 | 状态 |
|---|---|
| `crates/owo-agent-core/src/sandbox.rs`（新建） | ✅ |
| `crates/owo-agent-core/src/credentials.rs`（新建） | ✅ |
| `crates/owo-agent-core/src/audit_chain.rs`（新建） | ✅ |
| `crates/owo-agent-core/tests/sandbox_tests.rs`（新建） | ✅ |
| `crates/owo-agent-core/tests/credentials_tests.rs`（新建） | ✅ |
| `crates/owo-agent-core/tests/audit_chain_tests.rs`（新建） | ✅ |
| `.coord6/STATUS-agent3.md`、`.coord6/DEPENDENCIES-agent3.md` | ✅ |

未修改 `lib.rs`（由主控统一导出）；未修改其他 core 模块；未改 Cargo.toml（零新依赖）。

## 完成清单

### 子任务 1：sandbox.rs（X01）
- `SandboxPolicy { name, workspace, file_scope, network_policy, cpu_ms, mem_mb, ttl_secs, allow_hosts, require_isolation, allow_degraded, deny_programs, allow_unrestricted_file, allow_unrestricted_network }`；
- `SandboxPolicy::validate()`：越界组合拒绝——Unrestricted 文件/网络必须显式放行、AllowList 需要非空 allow_hosts、cpu_ms=0 非法、WorkspaceOnly 必须带 workspace；
- `SandboxCommand::validate()`：策略自检 + cwd 工作区越界 + 危险程序黑名单（deny 优先，大小写不敏感）；
- `SandboxExecutor` trait（`spawn`/`kill`/`check_healthy`/`capability`/`name`）+ `MockSandboxExecutor` 测试替身；
- `PlatformSupport` + `probe_platform_support()`（Wave 1 保守探测：无法验证即不可用，不假装安全）+ `evaluate_capability()`（Full / 显式 Degraded / 显式 Unsupported）；
- `SandboxManager`：策略校验 → 能力评估 → 降级/拒绝 → 审计事件的统一入口；`SandboxAuditLog`（append-only，可汇入 audit_chain）；
- 本轮不改 `tools.rs/plugin.rs/mcp.rs` 调用链，只提供可替换抽象与门禁（下一轮 OS 接入点已在 DEPENDENCIES 留言）。

### 子任务 2：credentials.rs（X02）
- `ApiKeyRef { store_key, env_var, inline }`：引用型凭据模型，`inline` 序列化时跳过 → settings.json 契约成立；
- `ProviderConfig.api_key_ref` + `serialized_without_plaintext()`；
- `CredentialStore` trait + `MemoryCredentialStore`（测试替身，默认可用）+ `UnavailableStore`（显式不可用）；
- `windows_credential_manager()`：Wave 1 显式返回不可用（不静默假装），Wave 2 接入 Windows Credential Manager；
- `CredentialResolver::resolve()`：优先级 OS 凭据库 → 环境变量 → 显式内联（测试用）→ `CredentialError::Missing`（缺凭据优雅降级，不 panic）；
- `scan_json_for_secrets()`：settings.json 明文扫描门禁工具。

### 子任务 3：audit_chain.rs（X04）
- append-only 审计条目：`AuditRecord.seq` 由链分配、单调自增（外部伪造 seq 被覆盖）；
- 分段 HMAC-SHA256 链（`sha2` 既有依赖就地实现，零新依赖）：`hash(n) = HMAC(key, prev_hash(n) ‖ canonical(record(n)))`，每 `segment_len` 条锚定一次（`Anchor`）；
- `verify` / `verify_export` 可检出任意篡改：改字段（detail/actor/tool）、删记录（序号跳变/前驱断裂）、重排、伪造插入、锚点篡改/缺失、整体重链（锚点仍指向旧哈希）、错误 key；
- `AuditExport` + `export_to_file`/`load_export`/`verify_file`：导出附带链，可离线校验；
- `owo-agent audit verify|export` CLI 骨架：`AuditCliCommand`/`AuditCliOutcome`/`run_audit_cli`（仅模块，不接 main）。

## 门禁实测

| 门禁 | 结果 |
|---|---|
| `cargo fmt -p owo-agent-core -- --check` | ✅ 干净 |
| `cargo clippy -p owo-agent-core --all-targets -- -D warnings` | ✅ 0 警告 |
| `cargo test -p owo-agent-core --test sandbox_tests --test credentials_tests --test audit_chain_tests` | ✅ 50/50 |

新增用例：sandbox **19** + credentials **11** + audit_chain **20** = **50**（目标 ≥18，超 2.7 倍）。

## 需主控接线的点

1. `crates/owo-agent-core/src/lib.rs`：`pub mod sandbox;`、`pub mod credentials;`、`pub mod audit_chain;` 及 `pub use` 导出（三个模块均为自包含文件，可直接声明）。
2. 三个模块文件顶部有 `#![allow(dead_code)]`：`#[path]` 独立编译时的过渡手段，主控并入 lib.rs 后可视情况移除（pub 项在 lib 内不再判 dead）。
3. `owo-agent audit verify|export` 子命令接入 `crates/owo-agent-cli/src/main.rs`（本轮按指令不接 main；CLI 骨架入口 `run_audit_cli`）。
4. 全量门禁时运行 `cargo test -p owo-agent-core`（含本三件套 50 例）。

## 风险 / 未做项

- `probe_platform_support()` 为 Wave 1 保守占位：Windows 真实 AppContainer/Job Object 探测与创建、`SandboxExecutor` 的 OS 实现（tools/plugin/mcp 调用链改造）在下一轮（需 Cargo.toml 增 `Win32_Security_Credentials` feature 时另行审批）。
- `windows_credential_manager()` 显式不可用（Wave 2）；主密钥/审计链 key 的托管建议放凭据库（避免与导出文件同盘明文共存）。
- 沙箱审计事件（`SandboxAuditLog`）与 audit_chain 的对接（record → AuditRecord）留待主控接线。
- 并行观察：门禁期间 Agent 2 的 blackboard/critic/fleet 曾处于编译错误状态（lib 阻塞，非本 Agent 文件）；本 Agent 全程只读他人文件，最终全量门禁通过时他人文件已修复。

## 自查

- 6 个文件 UTF-8（无 BOM）实测通过；未引入新依赖；未改 lib.rs/Cargo.toml；未跑 `cargo test --workspace`（并行门禁约定）。
