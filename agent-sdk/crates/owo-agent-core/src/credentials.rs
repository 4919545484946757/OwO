//! 模型凭据安全抽象（综合文档 §6 P0 / X02，Wave 2：Windows Credential Manager 落地）。
// R11:credentials 质量收尾完成（rotate_managed_dek 轮换失败回滚）
//!
//! - `ProviderConfig.apiKeyRef` 引用型凭据模型：配置只存**引用**（凭据库条目名 / 环境变量名），
//!   永不内联明文；`settings.json` 契约：**永不出现明文密钥**（序列化时跳过内联字段）。
//! - `CredentialStore` trait + 测试替身 + `WindowsCredentialManagerStore`（raw FFI，
//!   零新依赖：CredWriteW/CredReadW/CredDeleteW）。
//! - 解析优先级：**OS 凭据库 → 环境变量 → 显式内联（测试用）→ 显式错误**
//!   （缺凭据优雅降级，禁止 panic）。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// 凭据库条目命名空间（Windows Credential Manager target 前缀）。
pub const CREDENTIAL_NAMESPACE: &str = "owo-agent/";

/// 凭据错误：显式报告缺失/不可用，禁止静默。
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum CredentialError {
    #[error("凭据缺失：{0}")]
    Missing(String),
    #[error("凭据库不可用：{0}")]
    StoreUnavailable(String),
    #[error("凭据库操作失败：{0}")]
    Store(String),
}

/// API Key 引用：只存引用，不存明文。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiKeyRef {
    /// 系统凭据库条目名（如 Windows Credential Manager 通用凭据名）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store_key: Option<String>,
    /// 环境变量名（如 `OPENAI_API_KEY`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_var: Option<String>,
    /// 内联明文：**仅测试/显式本地开发用**；序列化时永不写入（`settings.json` 契约）。
    #[serde(skip)]
    pub inline: Option<String>,
}

impl ApiKeyRef {
    pub fn from_env(var: &str) -> Self {
        Self {
            env_var: Some(var.to_string()),
            ..ApiKeyRef::default()
        }
    }

    pub fn from_store(key: &str) -> Self {
        Self {
            store_key: Some(key.to_string()),
            ..ApiKeyRef::default()
        }
    }

    pub fn inline(secret: &str) -> Self {
        Self {
            inline: Some(secret.to_string()),
            ..ApiKeyRef::default()
        }
    }

    /// 是否有可用引用（凭据库条目或环境变量）。
    pub fn has_refs(&self) -> bool {
        self.store_key.is_some() || self.env_var.is_some()
    }
}

/// Provider 配置：凭据一律经 `api_key_ref` 引用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderConfig {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_ref: Option<ApiKeyRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl ProviderConfig {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            api_key_ref: None,
            base_url: None,
            model: None,
        }
    }

    pub fn with_env_key(name: impl Into<String>, env_var: &str) -> Self {
        Self {
            name: name.into(),
            api_key_ref: Some(ApiKeyRef::from_env(env_var)),
            base_url: None,
            model: None,
        }
    }

    pub fn with_store_key(name: impl Into<String>, store_key: &str) -> Self {
        Self {
            name: name.into(),
            api_key_ref: Some(ApiKeyRef::from_store(store_key)),
            base_url: None,
            model: None,
        }
    }

    /// 序列化（`settings.json` 落地形态）：内联明文被跳过，永不写入。
    pub fn serialized_without_plaintext(&self) -> Result<String, CredentialError> {
        serde_json::to_string(self).map_err(|error| CredentialError::Store(error.to_string()))
    }
}

/// 凭据库接口（OS 接入点：Windows Credential Manager / DPAPI / keyring）。
pub trait CredentialStore: Send + Sync {
    fn name(&self) -> &'static str;
    fn available(&self) -> bool;
    fn get(&self, key: &str) -> Option<String>;
    fn set(&self, key: &str, secret: &str) -> Result<(), CredentialError>;
    fn delete(&self, key: &str) -> Result<(), CredentialError>;
    /// 轮换：写新值 → 读回校验；set 失败或校验不一致返回显式错误（不静默）。
    /// 语义：同 key 覆盖（set 原子性保证失败不破坏旧值）。
    fn rotate(&self, key: &str, new_secret: &str) -> Result<(), CredentialError> {
        if !self.available() {
            return Err(CredentialError::StoreUnavailable(
                "凭据库不可用，拒绝轮换".to_string(),
            ));
        }
        self.set(key, new_secret)?;
        match self.get(key) {
            Some(stored) if stored == new_secret => Ok(()),
            _ => Err(CredentialError::Store(
                "轮换校验失败：读回值不一致（已写入但不可读，建议删除后重写）".to_string(),
            )),
        }
    }
}

/// 测试替身：内存凭据库。
#[derive(Debug)]
pub struct MemoryCredentialStore {
    inner: Arc<Mutex<HashMap<String, String>>>,
    available_flag: bool,
}

impl Default for MemoryCredentialStore {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            available_flag: true,
        }
    }
}

impl MemoryCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn seeded(entries: &[(&str, &str)]) -> Self {
        let store = Self::new();
        for (key, secret) in entries {
            store.set(key, secret).unwrap();
        }
        store
    }

    pub fn with_available(available: bool) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            available_flag: available,
        }
    }
}

impl CredentialStore for MemoryCredentialStore {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn available(&self) -> bool {
        self.available_flag
    }

    fn get(&self, key: &str) -> Option<String> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(key)
            .cloned()
    }

    fn set(&self, key: &str, secret: &str) -> Result<(), CredentialError> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key.to_string(), secret.to_string());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), CredentialError> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(key);
        Ok(())
    }
}

/// 显式不可用的凭据库：返回明确原因，禁止假装可用。
#[derive(Debug)]
pub struct UnavailableStore {
    pub reason: &'static str,
}

impl CredentialStore for UnavailableStore {
    fn name(&self) -> &'static str {
        "unavailable"
    }

    fn available(&self) -> bool {
        false
    }

    fn get(&self, _key: &str) -> Option<String> {
        None
    }

    fn set(&self, _key: &str, _secret: &str) -> Result<(), CredentialError> {
        Err(CredentialError::StoreUnavailable(self.reason.to_string()))
    }

    fn delete(&self, _key: &str) -> Result<(), CredentialError> {
        Err(CredentialError::StoreUnavailable(self.reason.to_string()))
    }
}

/// Windows Credential Manager 后端（Wave 2：raw FFI，零新依赖）。
/// 条目 target 名带 `owo-agent/` 命名空间；失败返回显式错误，不静默。
#[cfg(target_os = "windows")]
pub struct WindowsCredentialManagerStore {
    available_flag: bool,
}

#[cfg(target_os = "windows")]
impl WindowsCredentialManagerStore {
    pub fn new() -> Self {
        Self {
            available_flag: win_cred::probe_available(),
        }
    }

    pub fn probe_result(&self) -> String {
        if self.available_flag {
            "Windows Credential Manager 可用".to_string()
        } else {
            format!(
                "Windows Credential Manager 不可用（错误 {}）",
                win_cred::last_error()
            )
        }
    }
}

#[cfg(target_os = "windows")]
impl Default for WindowsCredentialManagerStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Windows Credential Manager 接入点：返回真实可用实现（Windows）或显式不可用（其他平台）。
pub fn windows_credential_manager() -> Box<dyn CredentialStore> {
    #[cfg(target_os = "windows")]
    {
        Box::new(WindowsCredentialManagerStore::new())
    }
    #[cfg(not(target_os = "windows"))]
    {
        Box::new(UnavailableStore {
            reason: "Windows Credential Manager 仅支持 Windows",
        })
    }
}

#[cfg(target_os = "windows")]
impl CredentialStore for WindowsCredentialManagerStore {
    fn name(&self) -> &'static str {
        "windows-credential-manager"
    }

    fn available(&self) -> bool {
        self.available_flag
    }

    fn get(&self, key: &str) -> Option<String> {
        if !self.available_flag {
            return None;
        }
        win_cred::read(&format!("{CREDENTIAL_NAMESPACE}{key}"))
    }

    fn set(&self, key: &str, secret: &str) -> Result<(), CredentialError> {
        if !self.available_flag {
            return Err(CredentialError::StoreUnavailable(self.probe_result()));
        }
        win_cred::write(&format!("{CREDENTIAL_NAMESPACE}{key}"), secret)
    }

    fn delete(&self, key: &str) -> Result<(), CredentialError> {
        if !self.available_flag {
            return Err(CredentialError::StoreUnavailable(self.probe_result()));
        }
        win_cred::delete(&format!("{CREDENTIAL_NAMESPACE}{key}"))
    }
}

#[cfg(target_os = "windows")]
mod win_cred {
    //! Windows Credential Manager raw FFI（advapi32，零新依赖）。
    #![allow(clippy::upper_case_acronyms)]

    use super::CredentialError;
    use std::ffi::c_void;

    pub type BOOL = i32;
    pub type DWORD = u32;
    pub type LPWSTR = *mut u16;
    pub type LPBYTE = *mut u8;

    pub const TRUE: BOOL = 1;
    pub const ERROR_NOT_FOUND: DWORD = 1168;
    pub const CRED_TYPE_GENERIC: DWORD = 1;
    pub const CRED_PERSIST_LOCAL_MACHINE: DWORD = 2;

    #[link(name = "advapi32")]
    extern "system" {
        fn CredWriteW(credential: *const CREDENTIALW, flags: DWORD) -> BOOL;
        fn CredReadW(
            target_name: LPWSTR,
            typ: DWORD,
            flags: DWORD,
            credential: *mut *mut CREDENTIALW,
        ) -> BOOL;
        fn CredDeleteW(target_name: LPWSTR, typ: DWORD, flags: DWORD) -> BOOL;
        fn CredFree(buffer: *mut c_void);
        fn GetLastError() -> DWORD;
    }

    #[repr(C)]
    pub struct CREDENTIALW {
        pub flags: DWORD,
        pub cred_type: DWORD,
        pub target_name: LPWSTR,
        pub comment: LPWSTR,
        pub last_written: [DWORD; 2],
        pub credential_blob_size: DWORD,
        pub credential_blob: LPBYTE,
        pub persist: DWORD,
        pub attribute_count: DWORD,
        pub attributes: *mut c_void,
        pub target_alias: LPWSTR,
        pub user_name: LPWSTR,
    }

    pub fn last_error() -> DWORD {
        unsafe { GetLastError() }
    }

    fn to_wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 能力探测：读一个不存在的条目应返回"未找到"（说明 API 可用）。
    pub fn probe_available() -> bool {
        let target = to_wide("owo-agent/__probe__");
        let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
        let ok = unsafe {
            CredReadW(
                target.as_ptr() as LPWSTR,
                CRED_TYPE_GENERIC,
                0,
                &mut credential,
            )
        };
        if ok == TRUE {
            unsafe {
                CredFree(credential as *mut c_void);
            }
            return true;
        }
        last_error() == ERROR_NOT_FOUND
    }

    pub fn read(target: &str) -> Option<String> {
        let wide_target = to_wide(target);
        let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
        let ok = unsafe {
            CredReadW(
                wide_target.as_ptr() as LPWSTR,
                CRED_TYPE_GENERIC,
                0,
                &mut credential,
            )
        };
        if ok != TRUE || credential.is_null() {
            return None;
        }
        let size = unsafe { (*credential).credential_blob_size as usize };
        let blob = unsafe { (*credential).credential_blob };
        let secret = if size > 0 && !blob.is_null() {
            let bytes = unsafe { std::slice::from_raw_parts(blob, size) };
            String::from_utf8_lossy(bytes).into_owned()
        } else {
            String::new()
        };
        unsafe {
            CredFree(credential as *mut c_void);
        }
        Some(secret)
    }

    pub fn write(target: &str, secret: &str) -> Result<(), CredentialError> {
        let wide_target = to_wide(target);
        let mut secret_bytes = secret.as_bytes().to_vec();
        let credential = CREDENTIALW {
            flags: 0,
            cred_type: CRED_TYPE_GENERIC,
            target_name: wide_target.as_ptr() as LPWSTR,
            comment: std::ptr::null_mut(),
            last_written: [0, 0],
            credential_blob_size: secret_bytes.len() as DWORD,
            credential_blob: secret_bytes.as_mut_ptr(),
            persist: CRED_PERSIST_LOCAL_MACHINE,
            attribute_count: 0,
            attributes: std::ptr::null_mut(),
            target_alias: std::ptr::null_mut(),
            user_name: std::ptr::null_mut(),
        };
        let ok = unsafe { CredWriteW(&credential, 0) };
        if ok != TRUE {
            return Err(CredentialError::Store(format!(
                "CredWriteW 失败（错误 {}）",
                last_error()
            )));
        }
        Ok(())
    }

    pub fn delete(target: &str) -> Result<(), CredentialError> {
        let wide_target = to_wide(target);
        let ok = unsafe { CredDeleteW(wide_target.as_ptr() as LPWSTR, CRED_TYPE_GENERIC, 0) };
        if ok != TRUE {
            return Err(CredentialError::Store(format!(
                "CredDeleteW 失败（错误 {}）",
                last_error()
            )));
        }
        Ok(())
    }
}

/// 凭据解析器：OS 凭据库 → 环境变量 → 显式错误。
pub struct CredentialResolver {
    store: Box<dyn CredentialStore>,
}

impl CredentialResolver {
    pub fn new(store: Box<dyn CredentialStore>) -> Self {
        Self { store }
    }

    /// 测试/本地：内存凭据库。
    pub fn with_memory_store() -> Self {
        Self::new(Box::<MemoryCredentialStore>::default())
    }

    /// 无凭据库：仅环境变量路径。
    pub fn no_store() -> Self {
        Self::new(Box::new(UnavailableStore {
            reason: "未配置凭据库",
        }))
    }

    pub fn store(&self) -> &dyn CredentialStore {
        self.store.as_ref()
    }

    /// 解析优先级：OS 凭据库 → 环境变量 → 显式内联（测试用）→ 显式错误。
    pub fn resolve(&self, api_key_ref: &ApiKeyRef) -> Result<String, CredentialError> {
        if let Some(key) = api_key_ref.store_key.as_deref() {
            if self.store.available() {
                if let Some(secret) = self.store.get(key) {
                    return Ok(secret);
                }
            }
        }
        if let Some(var) = api_key_ref.env_var.as_deref() {
            match std::env::var(var) {
                Ok(value) if !value.is_empty() => return Ok(value),
                _ => {}
            }
        }
        if let Some(secret) = api_key_ref.inline.as_deref() {
            return Ok(secret.to_string());
        }
        Err(CredentialError::Missing(describe_ref(api_key_ref)))
    }
}

fn describe_ref(api_key_ref: &ApiKeyRef) -> String {
    let mut parts = Vec::new();
    if let Some(key) = &api_key_ref.store_key {
        parts.push(format!("store_key={}", key));
    }
    if let Some(var) = &api_key_ref.env_var {
        parts.push(format!("env_var={}", var));
    }
    if parts.is_empty() {
        "未配置任何凭据引用".to_string()
    } else {
        format!("未找到可用凭据（{}）", parts.join("、"))
    }
}

/// 明文扫描：检测 JSON 字符串是否泄漏给定密钥（契约测试/门禁用）。
/// 返回命中的密钥名列表（空 = 无泄漏）。
pub fn scan_json_for_secrets(json: &str, secrets: &[&str]) -> Vec<String> {
    secrets
        .iter()
        .filter(|secret| !secret.is_empty() && json.contains(**secret))
        .map(|secret| (*secret).to_string())
        .collect()
}

// ---------- DEK 托管（R9：存储加密密钥经凭据库托管，与导出文件分离） ----------

/// DEK 长度（32 字节）。
pub const MANAGED_DEK_LEN: usize = 32;

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{:02x}", byte)).collect()
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    for chunk in text.as_bytes().chunks(2) {
        let high = (chunk[0] as char).to_digit(16)? as u8;
        let low = (chunk[1] as char).to_digit(16)? as u8;
        out.push((high << 4) | low);
    }
    Some(out)
}

/// 托管 DEK：从凭据库读取 32 字节密钥（hex 编码）；不存在则生成并写入。
/// 凭据库不可用 → 显式错误（不静默降级到未托管密钥）。
pub fn managed_dek(
    store: &dyn CredentialStore,
    store_key: &str,
) -> Result<[u8; MANAGED_DEK_LEN], CredentialError> {
    if !store.available() {
        return Err(CredentialError::StoreUnavailable(
            "DEK 托管凭据库不可用（拒绝未托管密钥的静默降级）".to_string(),
        ));
    }
    let dek = match store.get(store_key) {
        Some(stored) => {
            let bytes = hex_decode(&stored).ok_or_else(|| {
                CredentialError::Store("托管 DEK 损坏（非合法十六进制）".to_string())
            })?;
            if bytes.len() != MANAGED_DEK_LEN {
                return Err(CredentialError::Store(format!(
                    "托管 DEK 长度非法：{}（期望 {MANAGED_DEK_LEN}）",
                    bytes.len()
                )));
            }
            let mut dek = [0u8; MANAGED_DEK_LEN];
            dek.copy_from_slice(&bytes);
            dek
        }
        None => {
            let mut dek = [0u8; MANAGED_DEK_LEN];
            for chunk in dek.chunks_mut(16) {
                let uuid = uuid::Uuid::new_v4();
                chunk.copy_from_slice(&uuid.as_bytes()[..chunk.len()]);
            }
            store
                .set(store_key, &hex_encode(&dek))
                .map_err(|error| CredentialError::Store(format!("托管 DEK 写入失败：{error}")))?;
            dek
        }
    };
    Ok(dek)
}

/// 轮换托管 DEK：生成新密钥并经凭据库轮换（覆盖 + 读回校验）。
/// 前置校验（R10）：旧 DEK 必须存在且可读（轮换 ≠ 初始化）；
/// 失败回滚（R11）：先备份旧值，rotate 失败（含读回校验不一致）时写回旧值，
/// 确保不出现"旧 DEK 已破坏、新 DEK 未生效"的悬空状态；返回显式错误。
/// 轮换成功后旧密文不可解密（预期语义）；返回新 DEK。
pub fn rotate_managed_dek(
    store: &dyn CredentialStore,
    store_key: &str,
) -> Result<[u8; MANAGED_DEK_LEN], CredentialError> {
    if !store.available() {
        return Err(CredentialError::StoreUnavailable(
            "DEK 托管凭据库不可用（拒绝轮换）".to_string(),
        ));
    }
    verify_managed_dek(store, store_key).map_err(|error| {
        CredentialError::Store(format!("轮换前置校验失败（旧 DEK 缺失或损坏）：{error}"))
    })?;
    // R11：备份旧值，轮换失败时回滚（写回旧值），禁止静默丢失。
    let old_value = store
        .get(store_key)
        .ok_or_else(|| CredentialError::Store("轮换备份失败：旧 DEK 读取不到".to_string()))?;
    let mut dek = [0u8; MANAGED_DEK_LEN];
    for chunk in dek.chunks_mut(16) {
        let uuid = uuid::Uuid::new_v4();
        chunk.copy_from_slice(&uuid.as_bytes()[..chunk.len()]);
    }
    let rotated = store.rotate(store_key, &hex_encode(&dek));
    if let Err(error) = rotated {
        // 回滚：尽力写回旧值。若回滚也失败，旧值可能已丢失——显式报告。
        let rollback = store
            .set(store_key, &old_value)
            .map_err(|rb| CredentialError::Store(format!("轮换失败回滚也失败：{rb}")));
        return match rollback {
            Ok(()) => Err(CredentialError::Store(format!(
                "托管 DEK 轮换失败（{error}），已回滚旧值"
            ))),
            Err(rollback_error) => Err(rollback_error),
        };
    }
    Ok(dek)
}

/// 验证托管 DEK 可读且合法（R10：轮换前/启动时校验；失败返回显式错误）。
pub fn verify_managed_dek(
    store: &dyn CredentialStore,
    store_key: &str,
) -> Result<(), CredentialError> {
    if !store.available() {
        return Err(CredentialError::StoreUnavailable(
            "DEK 托管凭据库不可用".to_string(),
        ));
    }
    let stored = store
        .get(store_key)
        .ok_or_else(|| CredentialError::Store(format!("托管 DEK 缺失（{store_key} 未初始化）")))?;
    let bytes = hex_decode(&stored)
        .ok_or_else(|| CredentialError::Store("托管 DEK 损坏（非合法十六进制）".to_string()))?;
    if bytes.len() != MANAGED_DEK_LEN {
        return Err(CredentialError::Store(format!(
            "托管 DEK 长度非法：{}（期望 {MANAGED_DEK_LEN}）",
            bytes.len()
        )));
    }
    Ok(())
}
