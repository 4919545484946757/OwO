# OwO agent-sdk P0 落盘加密整改（Agent 1）

## 认领记录（2026-08-20）

- **任务**：P0 落盘加密整改 v3(XOR+HMAC) → v4(AES-256-GCM)，保留 v1/v2/v3 只读兼容，DPAPI 保护 DEK，补齐契约测试与安全说明。
- **独占文件**：
  - `agent-sdk/crates/owo-agent-core/src/storage_crypto.rs`
  - `agent-sdk/crates/owo-agent-core/src/storage_crypto_test.rs`、`storage_crypto_tests.rs`（历史上游离未挂载，整改后清理并内置 `#[cfg(test)] mod tests`）
  - `agent-sdk/Cargo.toml`、`agent-sdk/Cargo.lock`（如需：新增 `hmac` 直接依赖）
  - 如必要：`agent-sdk/SECURITY.md`
- **不触碰**：server、workflow、sandbox、CI 文件；不提交 git。

## 现状核实

- `storage_crypto.rs`、两个测试文件在 git 中均未跟踪（untracked），未挂载到 `lib.rs`，当前不参与编译。
- 工作树内 AES-GCM「半成品」存在版本号冲突与 v2/v3 读到错位：`encrypt_file_envelope`（DPAPI 路径，settings/session/sqlite_store 在用）写魔数字节 4 与 DEK v4 冲突；`decrypt_v2/v3` 复用已被改成 AES-GCM 的 `decrypt_with_dek`，真实旧 XOR 信封无法读。
- 整改方向：重写该模块为版本号语义自洽的全新实现；v1=DPAPI 自管理信封（生产继续用于 settings/session，写版本 1）；v2/v3=过渡 XOR 信封（只读迁移兼容，禁止再生成，XOR 仅存在于迁移读取路径）；v4=AES-256-GCM（新写唯一格式）。

## 验收命令

```
cargo fmt --all -- --check
cargo clippy -p owo-agent-core --all-targets -- -D warnings
cargo test -p owo-agent-core storage_crypto
```

## 完成记录（2026-08-20）✅

### 改动清单

- **`crates/owo-agent-core/src/storage_crypto.rs`（重写 953 行）**：
  - 版本语义自洽化，消除原「半成品」的版本号冲突（`encrypt_file_envelope` 原写魔数字节 4 与 DEK v4 冲突；v2/v3 读到被误改成 AES-GCM 的 `decrypt_with_dek`）。
  - 生产新写入 **v4 信封**：`magic | 4 | dek_len | protected_dek | data_len | nonce(12) | AES-256-GCM(dek, plain)`。
    - nonce 由 CSPRNG（`OsRng`）生成，12 字节随机且每次唯一；
    - 认证/解密由 `aes-gcm` 标准 AEAD 完成（`Aead::encrypt/decrypt`），**不自行实现任何密码学原语**；
    - 错误 DEK、篡改 nonce/密文/标签一律显式 `Decrypt` 拒绝。
  - v2/v3 改为**只读迁移兼容**：保留历史二进制布局解码；v3 认证校验改用标准 `hmac` crate（`Hmac<Sha256>` + `Mac::verify_slice` 常量时间比较），替换原手写 HMAC/`envelope_auth_tag`；XOR 派生流仅存在于 `decrypt_v2/v3` 迁移读取路径（`legacy_xor_crypt`，模块注释明确禁止再生成为 2/3）。
  - v1（DPAPI 自管理信封 `magic|1|dpapi(plain)`）保持 settings/session/sqlite/审计现行读写不变，写版本显式固定 v1，与新 v4 格式解耦。
  - 非 Windows 继续对 DPAPI 相关函数返回 `StorageCryptoError::Unsupported`，**禁止静默降级为明文**。
  - 全量边界检查（`checked_add`/显式长度校验），替换原 `expect("4 字节切片")` 潜在 panic。
  - `#[cfg(test)]` 内置 14 个契约测试。
- **测试文件整合**：删除历史游离未挂载的 `storage_crypto_test.rs` / `storage_crypto_tests.rs`（git 未跟踪、lib.rs 未注册），测试改为模块内 `#[cfg(test)] mod tests`，随 `cargo test -p owo-agent-core storage_crypto` 直接运行。
- **`crates/owo-agent-core/Cargo.toml`**：新增 `hmac = "0.12"` 直接依赖（lock 中已有 0.12.1，无新 fetch）。
- **`SECURITY.md`**：存储加密安全承诺更新——新写入只允许 v4 AES-256-GCM；v1/v2/v3 仅只读迁移、不得再生成；非 Windows 显式不可用。

### 兼容策略

| 版本 | 格式 | 写（新生成） | 读 |
|---|---|---|---|
| v1 | `magic\|1\|DPAPI(plain)` | ✅ 现行（settings/会话/审计 DPAPI 自管理） | ✅ |
| v2 | `magic\|2\|dek_len\|protected_dek\|XOR(data)` | ❌ 禁止 | ✅ 只读迁移 |
| v3 | `magic\|3\|…\|data_len\|XOR(data)\|HMAC-SHA256` | ❌ 禁止 | ✅ 只读迁移（HMAC 常量时间校验） |
| v4 | `magic\|4\|…\|nonce(12)\|AES-256-GCM` | ✅ 所有新 DEK 写入唯一格式 | ✅ |

- DEK 仍经 Windows DPAPI（`protect_dek`/`unprotect_dek`）保护，与备份/导出文件分离。
- `encrypt_file_envelope_with_dek` 读入口版本分派覆盖 1..=4，未知版本显式 `Format` 错误。
- 测试用兼容夹具按历史 wire 格式手工构造 v1/v2/v3 信封（v2/v3 经 `legacy_xor_crypt`/`legacy_v3_tag` 生成），验证只读兼容与篡改拒绝路径。

### 测试命令与结果（环境注：cargo 1.81 无法解析当前 lock 中 `idna_adapter 1.2.2`（需 edition2024，属库既有改动引入的预存问题），故用已安装的 `cargo 1.97(stable)` 验证）

```
cargo +stable fmt --all -- --check        → 通过（EXIT=0）
cargo +stable clippy -p owo-agent-core --all-targets -- -D warnings → 通过（EXIT=0，无警告）
cargo +stable test -p owo-agent-core storage_crypto → 通过 14/14
cargo +stable test -p owo-agent-core      → 全绿：303 lib + 全部集成套件全过
```

### 验收对照

1. ✅ AEAD 替换新写入 XOR：v4 = AES-256-GCM（`aes-gcm` 标准实现）。
2. ✅ 随机唯一 nonce：`OsRng` 12 字节；测试验证同明文两次加密 nonce 两两不同。
3. ✅ v1/v2/v3 只读解密兼容；新写入仅 v4（测试断言写出版本为 4）。
4. ✅ DPAPI 保护 DEK；非 Windows 显式 `Unsupported`，不静默降级。
5. ✅ 认证由标准 AEAD 常量时间完成；v3 迁移校验用 `hmac` crate，非手写原语。
6. ✅ 测试覆盖：round-trip（含空/多长度）、同明文两次密文不同、篡改任一区域（魔数/版本/DEK 段/data_len/nonce/密文首末/标签）必拒绝、错误 DEK 必拒绝、v1/v2/v3 兼容、新版本 round-trip。
7. ✅ 安全说明更新（模块 doc + 顶注释 + SECURITY.md）：v1/v2/v3 仅迁移兼容、不得再生成。