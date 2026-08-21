// owO agent-sdk P0 落盘加密整改（2026-08-20）：
// v4 AES-256-GCM 取代 v2/v3 过渡 XOR 流；v1/v2/v3 仅保留只读迁移兼容，不再生成。
//
// 版本语义（信封体内 magic 之后 version 字节之后的所有内容）：
// - v1 = DPAPI 自管理信封 `magic | 1 | DPAPI(明文)`（settings/session/sqlite_store 在用，正常读写 v1）。
// - v2 = 过渡 DEK 信封 `magic | 2 | DEK段 | XOR(明文, 派生流)`（无认证，只读迁移）。
// - v3 = 过渡 DEK 信封 `magic | 3 | DEK段 | data_len | XOR(明文) | HMAC-SHA256`（只读迁移）。
// - v4 = 生产 DEK 信封 `magic | 4 | DEK段 | data_len | nonce(12) | AES-256-GCM(dek, 明文)`（所有新写入唯一格式）。

//! 存储加密（综合文档 §6 P0 / X04 落盘加密）。
//!
//! Windows DPAPI（`CryptProtectData`/`CryptUnprotectData`，crypt32 raw FFI）保护数据加密密钥（DEK）：
//! - `encrypt_blob`/`decrypt_blob`：DPAPI 直接加解密（密钥由 OS 用户主密钥管理，密钥永不落盘）；
//! - `generate_dek`/`protect_dek`/`unprotect_dek`：显式 DEK 生命周期（32 字节随机 + DPAPI 保护）；
//! - `encrypt_file_envelope`/`decrypt_file_envelope`：v1 DPAPI 自管理文件信封（magic + version + blob）；
//! - `encrypt_file_envelope_with_dek`/`decrypt_file_envelope_with_dek`：v4 文件信封（AES-256-GCM，
//!   随机 nonce，常量时间 AEAD 认证；读取兼容 v1/v2/v3）；
//! - `encrypt_with_dek`/`decrypt_with_dek`：v4 载荷原语（nonce ‖ AES-256-GCM 密文）；
//! - `encrypt_sensitive_value`/`decrypt_sensitive_value`：敏感列加密（base64(DPAPI(value)) 形态）；
//! - 非 Windows：所有 OS 密钥保护/解密函数返回 `StorageCryptoError::Unsupported`，**禁止静默降级**。
//!
//! 安全说明：
//! - **生产新写入只允许 v4（AES-256-GCM）**：随机唯一 nonce、标准 AEAD 常量时间认证校验，
//!   使用 `aes-gcm` 标准实现，不自行实现任何密码学原语（认证由 AEAD 完成）；
//! - **v1/v2/v3 仅用于迁移兼容的只读解密**：v2 无认证、v3 仅 HMAC、均基于固定派生 XOR 流，
//!   机密性与认证强度不足，任何新写入（含迁移封存的重新落盘）必须重写为 v4，不得再生成为 2/3 版本；
//! - DEK 经 Windows DPAPI 保护，与导出/备份文件分离；非 Windows 显式拒绝，不静默降级为明文。

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use hmac::{Hmac, Mac};
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::path::Path;

/// 信封文件魔数（owo-crypt）。
pub const ENVELOPE_MAGIC: &[u8; 8] = b"OWOCRYPT";
/// 当前版本（最新格式，所有新写入使用）。
pub const ENVELOPE_VERSION: u8 = ENVELOPE_VERSION_V4_AEAD;
/// v1：DPAPI 自管理信封（magic + 1 + dpapi(plain)）。
pub const ENVELOPE_VERSION_V1_DPAPI: u8 = 1;
/// v2：过渡 DEK 信封（无认证，只读迁移）。
pub const ENVELOPE_VERSION_V2_LEGACY: u8 = 2;
/// v3：过渡 DEK 信封（HMAC-SHA256 认证，只读迁移）。
pub const ENVELOPE_VERSION_V3_LEGACY: u8 = 3;
/// v4：生产 DEK 信封（AES-256-GCM，随机 nonce）。
pub const ENVELOPE_VERSION_V4_AEAD: u8 = 4;
/// DEK 长度（32 字节 = AES-256 级）。
pub const DEK_LEN: usize = 32;
/// AES-GCM 随机 nonce 长度（96 位）。
const NONCE_LEN: usize = 12;
/// 过渡 v3 认证标签长度（HMAC-SHA256）。
const LEGACY_TAG_LEN: usize = 32;

/// 存储加密错误：显式 Unsupported / 失败，禁止静默。
#[derive(Debug, thiserror::Error)]
pub enum StorageCryptoError {
    #[error("存储加密不可用：{0}")]
    #[allow(dead_code)]
    Unsupported(String),
    #[error("加密失败：{0}")]
    Encrypt(String),
    #[error("解密失败：{0}")]
    Decrypt(String),
    #[error("信封格式错误：{0}")]
    Format(String),
    #[error("io 错误：{0}")]
    Io(#[from] std::io::Error),
}

/// 生成数据加密密钥（32 字节 CSPRNG 随机）。
pub fn generate_dek() -> [u8; DEK_LEN] {
    let mut dek = [0u8; DEK_LEN];
    OsRng.fill_bytes(&mut dek);
    dek
}

/// 加密明文（DPAPI，当前用户作用域，禁止 UI 交互）。
pub fn encrypt_blob(plain: &[u8]) -> Result<Vec<u8>, StorageCryptoError> {
    #[cfg(target_os = "windows")]
    {
        win_dpapi::protect(plain, None)
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err(StorageCryptoError::Unsupported(
            "DPAPI 存储加密仅支持 Windows（非 Windows 显式不可用，不静默降级）".to_string(),
        ))
    }
}

/// 解密密文（DPAPI）。
pub fn decrypt_blob(cipher: &[u8]) -> Result<Vec<u8>, StorageCryptoError> {
    #[cfg(target_os = "windows")]
    {
        win_dpapi::unprotect(cipher, None)
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err(StorageCryptoError::Unsupported(
            "DPAPI 存储加密仅支持 Windows（非 Windows 显式不可用，不静默降级）".to_string(),
        ))
    }
}

/// 用 DPAPI 保护 DEK（密钥信封：DEK 密文可落盘，明文仅内存）。
pub fn protect_dek(dek: &[u8; DEK_LEN]) -> Result<Vec<u8>, StorageCryptoError> {
    encrypt_blob(dek)
}

/// 解出 DEK（长度校验，防截断替换）。
pub fn unprotect_dek(protected: &[u8]) -> Result<[u8; DEK_LEN], StorageCryptoError> {
    let plain = decrypt_blob(protected)?;
    let plain_len = plain.len();
    if plain_len != DEK_LEN {
        return Err(StorageCryptoError::Decrypt(format!(
            "DEK 长度非法：{plain_len}（期望 {DEK_LEN}）"
        )));
    }
    let mut dek = [0u8; DEK_LEN];
    dek.copy_from_slice(&plain);
    Ok(dek)
}

/// v1 文件信封加密：`magic | 1 | dpapi(plain)`。
/// DPAPI 自管理信封（settings/会话/审计等敏感文件的现行落盘形态）。
pub fn encrypt_file_envelope(path: &Path, plain: &[u8]) -> Result<(), StorageCryptoError> {
    let blob = encrypt_blob(plain)?;
    write_envelope(path, ENVELOPE_VERSION_V1_DPAPI, &blob)
}

/// v1 文件信封解密（校验 magic/version）。
pub fn decrypt_file_envelope(path: &Path) -> Result<Vec<u8>, StorageCryptoError> {
    let (version, blob) = read_envelope(path)?;
    if version != ENVELOPE_VERSION_V1_DPAPI {
        return Err(StorageCryptoError::Format(format!(
            "DPAPI 信封版本不支持：{version}"
        )));
    }
    decrypt_blob(&blob)
}

/// v4 文件信封加密（生产格式，AES-256-GCM，随机 nonce）。
/// 布局：`magic | 4 | dek_len(LE) | protected_dek | data_len(LE) | nonce(12) | AEAD密文(含16字节标签)`。
/// 密钥与导出文件分离（DEK 明文仅内存）。解密兼容 v1/v2/v3。
pub fn encrypt_file_envelope_with_dek(
    path: &Path,
    plain: &[u8],
    dek: &[u8; DEK_LEN],
) -> Result<(), StorageCryptoError> {
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let cipher = Aes256Gcm::new(dek.into());
    let aead_cipher = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plain)
        .map_err(|e| StorageCryptoError::Encrypt(format!("AES-256-GCM 加密失败：{e}")))?;

    let protected_dek = protect_dek(dek)?;
    let mut envelope = Vec::with_capacity(
        ENVELOPE_MAGIC.len() + 1 + 4 + protected_dek.len() + 4 + NONCE_LEN + aead_cipher.len(),
    );
    envelope.extend_from_slice(ENVELOPE_MAGIC);
    envelope.push(ENVELOPE_VERSION);
    envelope.extend_from_slice(&(protected_dek.len() as u32).to_le_bytes());
    envelope.extend_from_slice(&protected_dek);
    envelope.extend_from_slice(&(aead_cipher.len() as u32).to_le_bytes());
    envelope.extend_from_slice(&nonce_bytes);
    envelope.extend_from_slice(&aead_cipher);
    std::fs::write(path, envelope)?;
    Ok(())
}

/// DEK 信封解密：v1 兼容（DPAPI 自管理）、v2/v3 只读迁移、v4 生产（篡改/损坏显式拒绝）。
pub fn decrypt_file_envelope_with_dek(
    path: &Path,
    dek: &[u8; DEK_LEN],
) -> Result<Vec<u8>, StorageCryptoError> {
    let envelope = std::fs::read(path)?;
    let (version, rest) = split_envelope(&envelope)?;
    match version {
        ENVELOPE_VERSION_V1_DPAPI => decrypt_blob(rest),
        ENVELOPE_VERSION_V2_LEGACY => decrypt_v2(rest, dek),
        ENVELOPE_VERSION_V3_LEGACY => decrypt_v3(rest, dek),
        ENVELOPE_VERSION_V4_AEAD => decrypt_v4(rest, dek),
        other => Err(StorageCryptoError::Format(format!(
            "信封版本不支持：{other}"
        ))),
    }
}

/// 从信封体头部切出 DEK 段，返回 `(protected_dek, 剩余部分)`。
fn take_dek_segment<'a>(
    rest: &'a [u8],
    what: &str,
) -> Result<(&'a [u8], &'a [u8]), StorageCryptoError> {
    if rest.len() < 4 {
        return Err(StorageCryptoError::Format(format!(
            "{what} 信封 DEK 段头不完整"
        )));
    }
    let mut len_bytes = [0u8; 4];
    len_bytes.copy_from_slice(&rest[..4]);
    let dek_len = u32::from_le_bytes(len_bytes) as usize;
    let end = 4usize
        .checked_add(dek_len)
        .ok_or_else(|| StorageCryptoError::Format(format!("{what} 信封 DEK 段长度溢出")))?;
    if rest.len() < end {
        return Err(StorageCryptoError::Format(format!(
            "{what} 信封 DEK 段不完整"
        )));
    }
    Ok((&rest[4..end], &rest[end..]))
}

fn read_u32(bytes: &[u8]) -> u32 {
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&bytes[..4]);
    u32::from_le_bytes(buf)
}

/// 校验存储的 DEK 与调用方提供的 DEK 一致（防止密钥轮换后旧密文被误解）。
fn require_dek_match(
    stored: &[u8; DEK_LEN],
    provided: &[u8; DEK_LEN],
) -> Result<(), StorageCryptoError> {
    if stored != provided {
        return Err(StorageCryptoError::Decrypt(
            "DEK 不匹配（密钥轮换后旧密文不可解）".to_string(),
        ));
    }
    Ok(())
}

/// 过渡 v2（无认证形态，只读迁移兼容旧导出）：DEK 匹配 + 固定派生 XOR 流解密。
/// 注意：v2 无认证标签，不满足生产安全要求，仅用于读取历史上已落盘的信封。
fn decrypt_v2(rest: &[u8], dek: &[u8; DEK_LEN]) -> Result<Vec<u8>, StorageCryptoError> {
    let (protected_dek, data) = take_dek_segment(rest, "v2")?;
    let stored_dek = unprotect_dek(protected_dek)?;
    require_dek_match(&stored_dek, dek)?;
    legacy_xor_crypt(data, dek)
}

/// 过渡 v3（HMAC 认证形态，只读迁移兼容）：DEK 匹配 + HMAC-SHA256 认证校验 + XOR 解密。
fn decrypt_v3(rest: &[u8], dek: &[u8; DEK_LEN]) -> Result<Vec<u8>, StorageCryptoError> {
    let (protected_dek, data) = take_dek_segment(rest, "v3")?;
    let stored_dek = unprotect_dek(protected_dek)?;
    require_dek_match(&stored_dek, dek)?;
    if data.len() < 4 {
        return Err(StorageCryptoError::Format(
            "v3 信封数据长度头不完整".to_string(),
        ));
    }
    let data_len = read_u32(&data[..4]) as usize;
    let body = &data[4..];
    let need = data_len
        .checked_add(LEGACY_TAG_LEN)
        .ok_or_else(|| StorageCryptoError::Format("v3 信封数据长度溢出".to_string()))?;
    if body.len() < need {
        return Err(StorageCryptoError::Format(
            "v3 信封数据段或认证标签不完整".to_string(),
        ));
    }
    let data_cipher = &body[..data_len];
    let stored_tag = &body[data_len..need];
    if !legacy_v3_verify(dek, protected_dek, data_cipher, stored_tag) {
        return Err(StorageCryptoError::Decrypt(
            "认证标签不匹配（备份包被篡改或损坏，拒绝解密）".to_string(),
        ));
    }
    legacy_xor_crypt(data_cipher, dek)
}

/// v4 生产解密（AES-256-GCM，标准 AEAD 常量时间认证校验；nonce/密文/标签任一篡改显式拒绝）。
fn decrypt_v4(rest: &[u8], dek: &[u8; DEK_LEN]) -> Result<Vec<u8>, StorageCryptoError> {
    let (protected_dek, data) = take_dek_segment(rest, "v4")?;
    let stored_dek = unprotect_dek(protected_dek)?;
    require_dek_match(&stored_dek, dek)?;
    if data.len() < 4 {
        return Err(StorageCryptoError::Format(
            "v4 信封数据长度头不完整".to_string(),
        ));
    }
    let data_len = read_u32(&data[..4]) as usize;
    let payload = &data[4..];
    let need = NONCE_LEN
        .checked_add(data_len)
        .ok_or_else(|| StorageCryptoError::Format("v4 信封数据长度溢出".to_string()))?;
    if payload.len() < need {
        return Err(StorageCryptoError::Format(
            "v4 信封数据段不完整".to_string(),
        ));
    }
    let nonce_bytes = &payload[..NONCE_LEN];
    let aead_cipher = &payload[NONCE_LEN..need];
    let cipher = Aes256Gcm::new(dek.into());
    cipher
        .decrypt(Nonce::from_slice(nonce_bytes), aead_cipher)
        .map_err(|_| {
            StorageCryptoError::Decrypt(
                "认证解密失败（密文/Nonce/标签被篡改或损坏，拒绝解密）".to_string(),
            )
        })
}

/// 过渡 v2/v3 的固定派生 XOR 流（仅存在于迁移读取路径与兼容测试夹具）。
/// 说明：该原语不满足生产机密性/认证要求，**仅供读取历史上已落盘的信封**；
/// 任何新写入必须走 v4 AES-256-GCM（`encrypt_file_envelope_with_dek` / `encrypt_with_dek`）。
fn legacy_xor_crypt(data: &[u8], dek: &[u8; DEK_LEN]) -> Result<Vec<u8>, StorageCryptoError> {
    let mut hasher = Sha256::new();
    hasher.update(dek);
    hasher.update(b"owo-envelope-stream-v2");
    let stream_key = hasher.finalize();
    Ok(data
        .iter()
        .enumerate()
        .map(|(i, byte)| byte ^ stream_key[i % stream_key.len()])
        .collect())
}

/// 过渡 v3 认证标签（HMAC-SHA256，标准 `hmac` 实现，非手写）。
/// 输入顺序保持与历史 v3 一致：`version ‖ dek_len ‖ protected_dek ‖ data_len ‖ data_cipher`。
/// 仅用于兼容测试构造旧 v3 夹具。
#[cfg(test)]
fn legacy_v3_tag(
    dek: &[u8; DEK_LEN],
    protected_dek: &[u8],
    data_cipher: &[u8],
) -> [u8; LEGACY_TAG_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(dek);
    hasher.update(b"owo-envelope-auth-v3");
    let auth_key = hasher.finalize();
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(&auth_key).expect("HMAC-SHA256 密钥长度恒合法");
    mac.update(&[ENVELOPE_VERSION_V3_LEGACY]);
    mac.update(&(protected_dek.len() as u32).to_le_bytes());
    mac.update(protected_dek);
    mac.update(&(data_cipher.len() as u32).to_le_bytes());
    mac.update(data_cipher);
    let mut out = [0u8; LEGACY_TAG_LEN];
    out.copy_from_slice(&mac.finalize().into_bytes());
    out
}

/// 过渡 v3 认证校验（`Mac::verify_slice` 常量时间比较）。
fn legacy_v3_verify(
    dek: &[u8; DEK_LEN],
    protected_dek: &[u8],
    data_cipher: &[u8],
    stored_tag: &[u8],
) -> bool {
    let mut hasher = Sha256::new();
    hasher.update(dek);
    hasher.update(b"owo-envelope-auth-v3");
    let auth_key = hasher.finalize();
    let mut mac = match <Hmac<Sha256> as Mac>::new_from_slice(&auth_key) {
        Ok(mac) => mac,
        Err(_) => return false,
    };
    mac.update(&[ENVELOPE_VERSION_V3_LEGACY]);
    mac.update(&(protected_dek.len() as u32).to_le_bytes());
    mac.update(protected_dek);
    mac.update(&(data_cipher.len() as u32).to_le_bytes());
    mac.update(data_cipher);
    mac.verify_slice(stored_tag).is_ok()
}

/// v4 载荷加密原语：返回 `nonce(12) ‖ AES-256-GCM(dek, plain)`。
pub fn encrypt_with_dek(plain: &[u8], dek: &[u8; DEK_LEN]) -> Result<Vec<u8>, StorageCryptoError> {
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let cipher = Aes256Gcm::new(dek.into());
    let aead_cipher = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plain)
        .map_err(|e| StorageCryptoError::Encrypt(format!("AES-256-GCM 加密失败：{e}")))?;
    let mut out = Vec::with_capacity(NONCE_LEN + aead_cipher.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&aead_cipher);
    Ok(out)
}

/// v4 载荷解密原语：认证失败（错误 DEK / 篡改）显式拒绝。
pub fn decrypt_with_dek(cipher: &[u8], dek: &[u8; DEK_LEN]) -> Result<Vec<u8>, StorageCryptoError> {
    if cipher.len() < NONCE_LEN {
        return Err(StorageCryptoError::Decrypt("密文长度不足".to_string()));
    }
    let nonce_bytes = &cipher[..NONCE_LEN];
    let aead_cipher = &cipher[NONCE_LEN..];
    let cipher_op = Aes256Gcm::new(dek.into());
    cipher_op
        .decrypt(Nonce::from_slice(nonce_bytes), aead_cipher)
        .map_err(|_| {
            StorageCryptoError::Decrypt(
                "认证解密失败（密文/Nonce/标签被篡改或损坏，拒绝解密）".to_string(),
            )
        })
}

fn write_envelope(path: &Path, version: u8, blob: &[u8]) -> Result<(), StorageCryptoError> {
    let mut envelope = Vec::with_capacity(ENVELOPE_MAGIC.len() + 1 + blob.len());
    envelope.extend_from_slice(ENVELOPE_MAGIC);
    envelope.push(version);
    envelope.extend_from_slice(blob);
    std::fs::write(path, envelope)?;
    Ok(())
}

fn read_envelope(path: &Path) -> Result<(u8, Vec<u8>), StorageCryptoError> {
    let envelope = std::fs::read(path)?;
    let (version, rest) = split_envelope(&envelope)?;
    Ok((version, rest.to_vec()))
}

/// 校验并切分信封头部：`(version, version 之后内容)`。
fn split_envelope(envelope: &[u8]) -> Result<(u8, &[u8]), StorageCryptoError> {
    if envelope.len() < ENVELOPE_MAGIC.len() + 1 {
        return Err(StorageCryptoError::Format("信封过短".to_string()));
    }
    if &envelope[..ENVELOPE_MAGIC.len()] != ENVELOPE_MAGIC {
        return Err(StorageCryptoError::Format("信封魔数不匹配".to_string()));
    }
    Ok((
        envelope[ENVELOPE_MAGIC.len()],
        &envelope[ENVELOPE_MAGIC.len() + 1..],
    ))
}

/// 导出脱敏原语（R10：备份/导出/诊断的 JSON 脱敏）：
/// - 敏感键（key/token/secret/authorization/password）值替换为 `***`；
/// - `messages[].content`（模型消息内容）截断为 120 字符；
/// - 其余结构原样保留（可审计性）。
pub fn redact_sensitive_json(value: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    fn redact_key(key: &str) -> bool {
        let lower = key.to_lowercase();
        lower.contains("key")
            || lower.contains("token")
            || lower.contains("secret")
            || lower.contains("authorization")
            || lower.contains("password")
            || lower.contains("apikey")
            || lower == "api_key"
    }
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, item)| {
                    let redacted = if redact_key(&key) {
                        Value::String("***".to_string())
                    } else if key == "messages" {
                        redact_messages(item)
                    } else {
                        redact_sensitive_json(item)
                    };
                    (key, redacted)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(redact_sensitive_json).collect()),
        other => other,
    }
}

fn redact_messages(value: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    const CONTENT_LIMIT: usize = 120;
    match value {
        Value::Array(messages) => Value::Array(
            messages
                .into_iter()
                .map(|message| match message {
                    Value::Object(mut map) => {
                        if let Some(Value::String(content)) = map.get("content").cloned() {
                            let truncated: String = content.chars().take(CONTENT_LIMIT).collect();
                            let mut out = if content.chars().count() > CONTENT_LIMIT {
                                format!("{truncated}…[截断]")
                            } else {
                                truncated
                            };
                            out = out
                                .replace("sk-", "sk-***")
                                .replace("Bearer ", "Bearer ***");
                            map.insert("content".to_string(), Value::String(out));
                        }
                        Value::Object(map)
                    }
                    other => other,
                })
                .collect(),
        ),
        other => other,
    }
}

/// 敏感列加密（settings.json 字段 / 会话 / 审计字段落盘形态：base64(dpapi(value))）。
pub fn encrypt_sensitive_value(value: &str) -> Result<String, StorageCryptoError> {
    let blob = encrypt_blob(value.as_bytes())?;
    Ok(BASE64.encode(blob))
}

/// 敏感列解密。
pub fn decrypt_sensitive_value(encoded: &str) -> Result<String, StorageCryptoError> {
    let cipher = BASE64
        .decode(encoded)
        .map_err(|error| StorageCryptoError::Format(error.to_string()))?;
    let plain = decrypt_blob(&cipher)?;
    String::from_utf8(plain)
        .map_err(|error| StorageCryptoError::Decrypt(format!("非 UTF-8：{error}")))
}

#[cfg(target_os = "windows")]
mod win_dpapi {
    #![allow(clippy::upper_case_acronyms, dead_code)]

    use super::StorageCryptoError;
    use std::ffi::c_void;

    pub type BOOL = i32;
    pub type DWORD = u32;

    pub const TRUE: BOOL = 1;
    pub const CRYPTPROTECT_UI_FORBIDDEN: DWORD = 0x1;

    #[repr(C)]
    pub struct DATA_BLOB {
        pub cb_data: DWORD,
        pub pb_data: *mut u8,
    }

    #[link(name = "crypt32")]
    extern "system" {
        fn CryptProtectData(
            p_data_in: *const DATA_BLOB,
            sz_data_descr: *const u16,
            p_optional_entropy: *const DATA_BLOB,
            pv_reserved: *mut c_void,
            p_prompt_struct: *mut c_void,
            dw_flags: DWORD,
            p_data_out: *mut DATA_BLOB,
        ) -> BOOL;
        fn CryptUnprotectData(
            p_data_in: *const DATA_BLOB,
            ppsz_data_descr: *mut *mut u16,
            p_optional_entropy: *const DATA_BLOB,
            pv_reserved: *mut c_void,
            p_prompt_struct: *mut c_void,
            dw_flags: DWORD,
            p_data_out: *mut DATA_BLOB,
        ) -> BOOL;
        fn LocalFree(h_mem: *mut c_void);
    }

    fn blob(data: &[u8]) -> DATA_BLOB {
        DATA_BLOB {
            cb_data: data.len() as DWORD,
            pb_data: data.as_ptr() as *mut u8,
        }
    }

    pub fn protect(plain: &[u8], _entropy: Option<&[u8]>) -> Result<Vec<u8>, StorageCryptoError> {
        let input = blob(plain);
        let mut output: DATA_BLOB = DATA_BLOB {
            cb_data: 0,
            pb_data: std::ptr::null_mut(),
        };
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok != TRUE || output.pb_data.is_null() {
            return Err(StorageCryptoError::Encrypt(
                "CryptProtectData 失败".to_string(),
            ));
        }
        let result =
            unsafe { std::slice::from_raw_parts(output.pb_data, output.cb_data as usize) }.to_vec();
        unsafe {
            LocalFree(output.pb_data as *mut c_void);
        }
        Ok(result)
    }

    pub fn unprotect(
        cipher: &[u8],
        _entropy: Option<&[u8]>,
    ) -> Result<Vec<u8>, StorageCryptoError> {
        let input = blob(cipher);
        let mut output: DATA_BLOB = DATA_BLOB {
            cb_data: 0,
            pb_data: std::ptr::null_mut(),
        };
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok != TRUE || output.pb_data.is_null() {
            return Err(StorageCryptoError::Decrypt(
                "CryptUnprotectData 失败（数据损坏或非当前用户加密）".to_string(),
            ));
        }
        let result =
            unsafe { std::slice::from_raw_parts(output.pb_data, output.cb_data as usize) }.to_vec();
        unsafe {
            LocalFree(output.pb_data as *mut c_void);
        }
        Ok(result)
    }
}

/// AES-GCM 认证标签长度（供测试断言实际密文口径使用）。
#[cfg(test)]
pub(crate) const AEAD_TAG_LEN: usize = 16;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_path(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("owo_crypto_{}_{}_{}", tag, std::process::id(), n))
    }

    /// 解析 v4 信封结构，返回 (dek 段长度, data_len 字段位置, nonce 起始, aead 密文起始, 文件总长)。
    fn v4_offsets(bytes: &[u8]) -> (usize, usize, usize, usize, usize) {
        let magic = ENVELOPE_MAGIC.len();
        let mut lb = [0u8; 4];
        lb.copy_from_slice(&bytes[magic + 1..magic + 5]);
        let dek_len = u32::from_le_bytes(lb) as usize;
        let dek_segment_start = magic + 1;
        let dek_segment_end = dek_segment_start + 4 + dek_len;
        let data_len_field = dek_segment_end;
        lb.copy_from_slice(&bytes[data_len_field..data_len_field + 4]);
        let _data_len = u32::from_le_bytes(lb) as usize;
        let nonce_start = data_len_field + 4;
        (
            dek_len,
            data_len_field,
            nonce_start,
            nonce_start + NONCE_LEN,
            bytes.len(),
        )
    }

    fn write_at(path: &PathBuf, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn v4_round_trip_varied_lengths() {
        let dek = generate_dek();
        let cases: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"A".to_vec(),
            b"Hello, World! AES-256-GCM round trip.".to_vec(),
            vec![0u8; 1024],
            (0u8..=255u8).collect(),
        ];
        for (i, plain) in cases.iter().enumerate() {
            let path = temp_path(&format!("rt{}", i));
            encrypt_file_envelope_with_dek(&path, plain, &dek).unwrap();
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(&bytes[..ENVELOPE_MAGIC.len()], ENVELOPE_MAGIC, "魔数");
            assert_eq!(
                bytes[ENVELOPE_MAGIC.len()],
                ENVELOPE_VERSION_V4_AEAD,
                "v4 版本"
            );
            let decrypted = decrypt_file_envelope_with_dek(&path, &dek).unwrap();
            assert_eq!(decrypted, *plain, "第 {i} 例 round-trip");
            std::fs::remove_file(&path).unwrap();
        }
    }

    #[test]
    fn v4_same_plaintext_produces_distinct_nonces() {
        let plain = b"same message, random nonce every time";
        let dek = generate_dek();
        let mut nonces = Vec::new();
        for i in 0..5 {
            let path = temp_path(&format!("nonce{}", i));
            encrypt_file_envelope_with_dek(&path, plain, &dek).unwrap();
            let bytes = std::fs::read(&path).unwrap();
            let (_, _, nonce_start, _, _) = v4_offsets(&bytes);
            nonces.push(bytes[nonce_start..nonce_start + NONCE_LEN].to_vec());
            std::fs::remove_file(&path).unwrap();
        }
        for i in 0..nonces.len() {
            for j in (i + 1)..nonces.len() {
                assert_ne!(nonces[i], nonces[j], "nonce 必须随机且两两不同");
            }
        }
    }

    #[test]
    fn v4_tamper_any_region_rejected() {
        let plain = b"tamper resistance payload for AES-GCM";
        let dek = generate_dek();
        let path = temp_path("tamper");
        encrypt_file_envelope_with_dek(&path, plain, &dek).unwrap();
        let original = std::fs::read(&path).unwrap();
        let (_, data_len_field, nonce_start, cipher_start, total) = v4_offsets(&original);
        // 篡改位点：魔数、版本、dek_len、protected_dek 首字节、data_len、nonce、密文首/末、末尾标签
        let mutation_indices = [
            0usize,                     // magic
            8usize,                     // version
            9usize,                     // dek_len
            13usize,                    // protected_dek[0]
            data_len_field,             // data_len
            nonce_start,                // nonce[0]
            cipher_start,               // 密文首字节
            total - 1,                  // 末尾标签字节
            (cipher_start + total) / 2, // 密文中部
        ];
        for (i, idx) in mutation_indices.iter().enumerate() {
            let mut corrupted = original.clone();
            corrupted[*idx] ^= 0xFF;
            let bad = temp_path(&format!("tamper{}", i));
            write_at(&bad, &corrupted);
            let result = decrypt_file_envelope_with_dek(&bad, &dek);
            assert!(result.is_err(), "篡改字节 @{} 应被拒绝", idx);
            std::fs::remove_file(&bad).unwrap();
        }
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn v4_wrong_dek_rejected() {
        let plain = b"wrong DEK must be rejected";
        let dek1 = generate_dek();
        let dek2 = generate_dek();
        let path = temp_path("wrongdek");
        encrypt_file_envelope_with_dek(&path, plain, &dek1).unwrap();
        let result = decrypt_file_envelope_with_dek(&path, &dek2);
        assert!(result.is_err(), "错误 DEK 必须拒绝");
        // 正确 DEK 仍可读
        let ok = decrypt_file_envelope_with_dek(&path, &dek1).unwrap();
        assert_eq!(ok, plain);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn v4_malformed_headers_rejected() {
        let dek = generate_dek();
        let p1 = temp_path("short");
        write_at(&p1, b"OWO");
        assert!(decrypt_file_envelope_with_dek(&p1, &dek).is_err());

        let p2 = temp_path("magic");
        let mut bytes = ENVELOPE_MAGIC.to_vec();
        bytes.push(ENVELOPE_VERSION_V4_AEAD);
        bytes.push(0);
        write_at(&p2, &bytes);
        assert!(decrypt_file_envelope_with_dek(&p2, &dek).is_err());

        let p3 = temp_path("ver");
        let mut bytes = ENVELOPE_MAGIC.to_vec();
        bytes.push(0x09);
        bytes.extend_from_slice(&[0, 0, 0, 0, 0]);
        write_at(&p3, &bytes);
        assert!(decrypt_file_envelope_with_dek(&p3, &dek).is_err());
        std::fs::remove_file(&p1).unwrap();
        std::fs::remove_file(&p2).unwrap();
        std::fs::remove_file(&p3).unwrap();
    }

    #[test]
    fn v1_dpapi_envelope_round_trip_and_version() {
        let plain = b"v1 dpapi self-managed envelope";
        let path = temp_path("v1");
        encrypt_file_envelope(&path, plain).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes[ENVELOPE_MAGIC.len()], ENVELOPE_VERSION_V1_DPAPI);
        let decrypted = decrypt_file_envelope(&path).unwrap();
        assert_eq!(decrypted, plain);
        // v1 也可经 with_dek 入口读（v1 分支不受 DEK 控制）
        let again = decrypt_file_envelope_with_dek(&path, &generate_dek()).unwrap();
        assert_eq!(again, plain);
        // v1 信封经 decrypt_file_envelope_with_dek 之外的反向联通：v4 信封不能走 v1 路径
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn dpapi_blob_round_trip() {
        let plain = b"DPAPI blob round trip payload";
        let blob = encrypt_blob(plain).unwrap();
        let back = decrypt_blob(&blob).unwrap();
        assert_eq!(back, plain);
    }

    #[test]
    fn dek_protect_unprotect_round_trip_and_wrong_len() {
        let dek = generate_dek();
        let protected = protect_dek(&dek).unwrap();
        let back = unprotect_dek(&protected).unwrap();
        assert_eq!(back, dek);
        // 非 DEK 长度的内容解出应显式拒绝
        let wrong = encrypt_blob(&[9u8; DEK_LEN - 1]).unwrap();
        assert!(unprotect_dek(&wrong).is_err());
    }

    #[test]
    fn v2_legacy_compat_read_only() {
        let plain = b"legacy v2 envelope (no auth)";
        let dek = generate_dek();
        let protected_dek = protect_dek(&dek).unwrap();
        let enc = legacy_xor_crypt(plain, &dek).unwrap();
        let mut envelope = Vec::new();
        envelope.extend_from_slice(ENVELOPE_MAGIC);
        envelope.push(ENVELOPE_VERSION_V2_LEGACY);
        envelope.extend_from_slice(&(protected_dek.len() as u32).to_le_bytes());
        envelope.extend_from_slice(&protected_dek);
        envelope.extend_from_slice(&enc);
        let path = temp_path("v2");
        write_at(&path, &envelope);
        let back = decrypt_file_envelope_with_dek(&path, &dek).unwrap();
        assert_eq!(back, plain);
        // 错误 DEK 拒绝
        assert!(decrypt_file_envelope_with_dek(&path, &generate_dek()).is_err());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn v3_legacy_compat_read_only_and_tamper_reject() {
        let plain = b"legacy v3 envelope (hmac auth)";
        let dek = generate_dek();
        let protected_dek = protect_dek(&dek).unwrap();
        let enc = legacy_xor_crypt(plain, &dek).unwrap();
        let tag = legacy_v3_tag(&dek, &protected_dek, &enc);
        let mut envelope = Vec::new();
        envelope.extend_from_slice(ENVELOPE_MAGIC);
        envelope.push(ENVELOPE_VERSION_V3_LEGACY);
        envelope.extend_from_slice(&(protected_dek.len() as u32).to_le_bytes());
        envelope.extend_from_slice(&protected_dek);
        envelope.extend_from_slice(&(enc.len() as u32).to_le_bytes());
        envelope.extend_from_slice(&enc);
        envelope.extend_from_slice(&tag);
        let path = temp_path("v3");
        write_at(&path, &envelope);
        let back = decrypt_file_envelope_with_dek(&path, &dek).unwrap();
        assert_eq!(back, plain);
        // 错误 DEK 拒绝
        assert!(decrypt_file_envelope_with_dek(&path, &generate_dek()).is_err());

        // 篡改密文字节 → HMAC 拒绝
        let mut corrupted = envelope.clone();
        let body_start = 8 + 1 + 4 + protected_dek.len() + 4;
        corrupted[body_start + 2] ^= 0xFF;
        let bad1 = temp_path("v3c");
        write_at(&bad1, &corrupted);
        assert!(decrypt_file_envelope_with_dek(&bad1, &dek).is_err());

        // 篡改认证标签 → 拒绝
        let mut corrupted = envelope.clone();
        let last = corrupted.len() - 1;
        corrupted[last] ^= 0xFF;
        let bad2 = temp_path("v3t");
        write_at(&bad2, &corrupted);
        assert!(decrypt_file_envelope_with_dek(&bad2, &dek).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(&bad1).unwrap();
        std::fs::remove_file(&bad2).unwrap();
    }

    #[test]
    fn new_writes_only_emit_production_versions() {
        let dek = generate_dek();
        let plain = b"new writes must be v4";
        let p1 = temp_path("onlyv4");
        encrypt_file_envelope_with_dek(&p1, plain, &dek).unwrap();
        let b1 = std::fs::read(&p1).unwrap();
        assert_eq!(b1[ENVELOPE_MAGIC.len()], ENVELOPE_VERSION_V4_AEAD);
        let p2 = temp_path("onlyv1");
        encrypt_file_envelope(&p2, plain).unwrap();
        let b2 = std::fs::read(&p2).unwrap();
        assert_eq!(b2[ENVELOPE_MAGIC.len()], ENVELOPE_VERSION_V1_DPAPI);
        // 没有任何新写入路径能产生 2/3 版本
        std::fs::remove_file(&p1).unwrap();
        std::fs::remove_file(&p2).unwrap();
    }

    #[test]
    fn v4_aead_payload_primitive_round_trip() {
        let dek = generate_dek();
        let plain = b"payload primitive round trip";
        let enc = encrypt_with_dek(plain, &dek).unwrap();
        assert_eq!(enc.len(), NONCE_LEN + plain.len() + AEAD_TAG_LEN);
        let back = decrypt_with_dek(&enc, &dek).unwrap();
        assert_eq!(back, plain);
        // 篡改拒绝
        let mut bad = enc.clone();
        let mid = bad.len() / 2;
        bad[mid] ^= 0xFF;
        assert!(decrypt_with_dek(&bad, &dek).is_err());
        // 错误 DEK 拒绝
        assert!(decrypt_with_dek(&enc, &generate_dek()).is_err());
    }

    #[test]
    fn sensitive_value_round_trip() {
        let original = "sk-prod-0123456789";
        let enc = encrypt_sensitive_value(original).unwrap();
        assert_ne!(enc, original);
        let back = decrypt_sensitive_value(&enc).unwrap();
        assert_eq!(back, original);
        // 非法 base64 显式拒绝
        assert!(decrypt_sensitive_value("!!!!not-base64!!!!").is_err());
    }

    #[test]
    fn redact_sensitive_json_keys_and_messages() {
        let value = json!({
            "client_id": "abc",
            "api_key": "sk-secret",
            "Authorization": "Bearer tok",
            "nested": {"token": "tok2", "keep": 1},
            "list": [{"secret": "s"}],
            "messages": [{"role": "assistant", "content": "short"}],
            "messages2": [{"role": "user", "content": "payload"}],
        });
        let redacted = redact_sensitive_json(value);
        assert_eq!(redacted["api_key"], "***");
        assert_eq!(redacted["Authorization"], "***");
        assert_eq!(redacted["nested"]["token"], "***");
        assert_eq!(redacted["nested"]["keep"], 1);
        assert_eq!(redacted["list"][0]["secret"], "***");
        assert_eq!(redacted["client_id"], "abc");
        assert_eq!(redacted["messages"][0]["content"], "short");
    }
}
