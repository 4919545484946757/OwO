# Agent 3 状态报告（安全边界黑盒契约测试）

## 任务
为现有安全边界建立「黑盒优先」的独立契约测试，不改业务实现。

## 交付物（2 个新测试文件 + 本报告）

1. `crates/owo-agent-server/tests/production_security_contract_tests.rs`
   —— 经 `build_router` 真实路由 + `tower::ServiceExt::oneshot` 内存往返的黑盒契约测试。
2. `crates/owo-agent-core/tests/production_security_contract_tests.rs`
   —— 存储 / 审计 / 凭据托管 / 沙箱最小安全语义契约测试（可选核心 crate，已创建）。

## 覆盖范围（对照验收目标）

### 1. 真实 router / 公开 API 行为（server 测试）
- `protected_routes_deny_anonymous_with_401`：11 个受保护面（/usage /sessions /skills
  /plugins /audit /settings /server/status /goal /session /settings/egress）未携带 token 一律 401。
- `auth_error_body_is_structured_and_token_free`：401 错误体带 `code=auth/unauthorized/not_retryable`，绝不回显 token。
- `public_surface_is_exactly_health_openapi_bootstrap`：仅 `/health`、`/openapi.json`、`/auth/token` 公开可达；
  错误 token 不得进入任何受保护面。
- `cors_whitelist_allows_loopback_and_rejects_malicious`：恶意 Origin（evil/attacker/malicious-site/
  局域网 IP/`null`）不回显 ACAO；webview + localhost/127.0.0.1 任意端口放行并回显。
- `rate_limit_returns_429_with_retry_after_and_never_bypasses_auth`：超全局 RPM → 429 + `Retry-After`；
  全新桶下匿名写请求放行后仍被鉴权拒绝 401（限流绝不绕过认证，绝不 2xx）。
- `secrets_never_leak_into_responses_or_audit`：token + 随机哨兵值在 /usage /settings /audit /skills
  /server/status、公开面、401 错误体、审计结构中均不出现；审计结构不含 `Bearer`。
- `settings_response_contains_no_plaintext_credentials`：/settings 真实往返非敏感字段；
  明文凭据字段（api_key/apikey/password/passwd/secret/credentials/access_key/private_key）绝不出现。

### 2. 存储与审计安全（core 测试）
- `provider_config_never_serializes_plaintext_secrets` / `settings_scan_detects_plaintext_leaks`：
  `ProviderConfig.serialized_without_plaintext` 跳过 inline 明文、`scan_json_for_secrets` 正向/负向闭环。
- `audit_chain_detects_any_tampering`：改 detail / 改 actor / 删记录 / 重排 / 篡改锚点 / 错误密钥，全部可检出。
- `audit_export_never_leaks_managed_key_or_secret` / `audit_managed_key_reuses_and_verifies_across_restart`：
  导出 JSON 不含托管密钥与摘要；密钥与导出文件分离。
- `credential_store_unavailable_fails_explicitly_no_settings_fallback` /
  `unavailable_store_never_writes_plaintext_back_to_settings`：
  凭据库不可用 → `from_managed_key`/`force_rotate_managed_key`/`managed_dek`/`resolve` 全部显式失败，
  序列化只保留引用、绝不回退写入 settings 明文。

### 3. 沙箱最小安全语义（core 测试）
- `sandbox_platform_probe_is_explicit_never_silent` / `unavailable_sandbox_reports_unsupported_explicitly`：
  不支持时显式报错 + 审计事件，绝不静默假装安全。
- `attach_failure_yields_no_guard_explicitly` / `attach_with_network_policy_is_rejected_before_attach`：
  不存在「挂接失败但子进程继续运行」的路径——attach/A 网络策略挂接失败一律显式 `Unsupported`，
  不发放 `JobGuard`，且不产生 `Attached` 审计。
- `windows_job_object_spawn_is_env_gated`：Windows Job Object 实证走环境门控
  （非 Windows / Job 不可用显式跳过并打印原因；`OWO_FORCE_OS_TESTS=1` 升级为失败）。

### 4. 数据与凭据约束
全部使用 `tempfile`（server）/ `std::env::temp_dir` + uuid（core）临时目录、
随机密钥 / 随机 token、`IdleProvider`（任何模型调用即失败），不使用真实模型或用户凭据。

## 验证结果（本地运行，工具链见下）
- `cargo fmt --all -- --check`：**通过**（仅本次新增两文件被格式化，未触碰他人文件）。
- `cargo test -p owo-agent-server --test production_security_contract_tests`：**7 passed, 0 failed**。
- `cargo test -p owo-agent-core --test production_security_contract_tests`：**12 passed, 0 failed**。
- `cargo clippy -p owo-agent-server|owo-agent-core --test production_security_contract_tests`：**0 warning**。

## 观察到的边界差异（已报告，未跨边界修改）
1. 中间件层序与注释不符：`owo-agent-server/src/lib.rs` 注释称「鉴权在最外层：未授权请求
   不进入限流，也不消耗令牌」，但实际 `.layer()` 顺序使 `enforce_rate_limit` 先于 `require_auth`
   执行。后果：未授权请求会消耗全局令牌桶，桶耗尽后匿名请求返回 429 而非 401。
   安全上不会绕过鉴权（匿名请求仍无法 2xx），但存在「未授权来源耗尽限流桶」的 DoS 面。
   已在测试 `rate_limit_..._never_bypasses_auth` 用「全新桶下匿名请求仍 401、绝不 2xx」固化正确契约，
   并明确不因层序差异误判。如需修复层序属业务改动，超出本 Agent 边界，提请主控/相关 Agent 评估。
2. 「token/凭据不进日志结构」的黑盒断言受限：`logging` 访问日志仅落 method/path/status（经代码确认
   `trace_id_middleware` 不落 body/header），暂无公开钩子可在黑盒测试内捕获 emit 的日志记录；
   本测试以「响应体 + 审计结构 + 401 错误体」三元覆盖该契约，日志结构部分以代码审查佐证。

## 边界遵守
未修改 `storage_crypto.rs`、CI 文件、`lib.rs` 或任何业务实现；未改动 Cargo / CI 配置；未提交 git。

## 环境备注
本机 `rust-toolchain.toml` 锁定的 `1.97.1` 在 rustup 中处于「部分安装」状态，`cargo` 反复触发
不完整的组件下载；实际 `stable` 通道已解析为 1.97.1 且可用，本次验证统一用 `cargo +stable` 绕过。
CI 若因此红，请检查 rustup 工具链缓存（见 `cargo_check.log` 等既有日志）。