// R11:lease 质量收尾完成
//! 租约与 fencing：worker/任务持有租约，写操作校验纪元号与 token，超时迁移/失败。
//!
//! 设计来源：《多Agent并行体系-生产级设计与跨机扩展-2026-08-16.md》§7 一致性：
//! - **租约**：默认 TTL 15s、可配；持有者心跳续租；到期即超时（迁移/失败）。
//! - **fencing**：acquire 递增纪元号（epoch）并签发 token；写操作必须校验
//!   `verify_write(holder, token, expected_epoch)`，旧 token/旧纪元一律拒绝
//!   （防止分区重连后双写）。
//! - **重连语义**：节点重连先 acquire 拿新 token，旧 token 作废；分区时控制面
//!   无法续租 → `read_only` 降级，写操作显式拒绝（不静默）。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 租约配置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseConfig {
    /// 租约 TTL（默认 15s）。
    pub ttl_secs: u64,
    /// 建议心跳续租间隔（默认 5s）。
    pub renew_interval_secs: u64,
}

impl Default for LeaseConfig {
    fn default() -> Self {
        Self {
            ttl_secs: 15,
            renew_interval_secs: 5,
        }
    }
}

/// 租约错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseError {
    UnknownHolder(String),
    BadToken {
        expected: String,
        got: String,
    },
    Expired(String),
    /// 写操作被 fencing 拒绝（分区/重连后旧纪元写入）。
    Fenced {
        holder: String,
        expected_epoch: u64,
        got_epoch: u64,
    },
    /// 控制面降级只读（无法续租/分区）。
    ReadOnly(String),
}

impl std::fmt::Display for LeaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownHolder(h) => write!(f, "未知租约持有者：{h}"),
            Self::BadToken { expected, got } => {
                write!(
                    f,
                    "token 不匹配（fencing 拒绝）：期望 {expected}，实际 {got}"
                )
            }
            Self::Expired(h) => write!(f, "租约已过期：{h}"),
            Self::Fenced {
                holder,
                expected_epoch,
                got_epoch,
            } => write!(
                f,
                "写操作被 fencing 拒绝：{holder} 期望纪元 {expected_epoch}，实际 {got_epoch}"
            ),
            Self::ReadOnly(h) => write!(f, "控制面降级只读，拒绝写：{h}"),
        }
    }
}

impl std::error::Error for LeaseError {}

/// 租约本体。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub holder: String,
    /// fencing token（每次 acquire/renew 签发；旧 token 作废）。
    pub token: String,
    /// 纪元号：每次 acquire 递增；写操作校验。
    pub epoch: u64,
    pub ttl: Duration,
    /// 过期时刻（相对进程时钟；由 `expires_at` 判定）。
    pub expires_at_unix_ms: u64,
    pub renewed_at_unix_ms: u64,
}

impl Lease {
    pub fn is_expired(&self, now_unix_ms: u64) -> bool {
        now_unix_ms >= self.expires_at_unix_ms
    }
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 租约管理器（Clone 共享同一租约表与纪元计数器）。
#[derive(Clone, Default)]
pub struct LeaseManager {
    config: LeaseConfig,
    leases: Arc<Mutex<HashMap<String, Lease>>>,
    epoch: Arc<AtomicU64>,
    /// 分区/续租失败时置只读：写操作显式拒绝。
    read_only: Arc<Mutex<bool>>,
}

impl std::fmt::Debug for LeaseManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaseManager")
            .field("config", &self.config)
            .field("epoch", &self.epoch.load(Ordering::Relaxed))
            .finish()
    }
}

impl LeaseManager {
    pub fn new() -> Self {
        Self::with_config(LeaseConfig::default())
    }

    pub fn with_config(config: LeaseConfig) -> Self {
        Self {
            config,
            leases: Arc::new(Mutex::new(HashMap::new())),
            epoch: Arc::new(AtomicU64::new(1)),
            read_only: Arc::new(Mutex::new(false)),
        }
    }

    /// 控制面降级只读（分区/无法续租时调用）。
    pub fn set_read_only(&self, ro: bool) {
        if let Ok(mut flag) = self.read_only.lock() {
            *flag = ro;
        }
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only.lock().map(|f| *f).unwrap_or(false)
    }

    pub fn current_epoch(&self) -> u64 {
        self.epoch.load(Ordering::Relaxed)
    }

    /// 持有者获取（或重连重取）租约：签发新 token 并递增纪元。
    /// 已存在的租约若未过期则保持原 token（续约），否则作废重签（fencing 迁移）。
    pub fn acquire(&self, holder: &str) -> Result<Lease, LeaseError> {
        let now = now_unix_ms();
        let mut leases = self
            .leases
            .lock()
            .map_err(|_| LeaseError::ReadOnly("锁异常".into()))?;
        if let Some(existing) = leases.get(holder) {
            if !existing.is_expired(now) {
                // 存活租约：重新签发 token（防旧 token 双写），epoch 不变。
                let renewed = Lease {
                    holder: holder.to_string(),
                    token: new_token(holder),
                    epoch: existing.epoch,
                    ttl: Duration::from_secs(self.config.ttl_secs),
                    expires_at_unix_ms: now + self.config.ttl_secs * 1000,
                    renewed_at_unix_ms: now,
                };
                leases.insert(holder.to_string(), renewed.clone());
                return Ok(renewed);
            }
            // 过期：作废（迁移语义），重新获取新 epoch。
        }
        let epoch = self.epoch.fetch_add(1, Ordering::Relaxed);
        let lease = Lease {
            holder: holder.to_string(),
            token: new_token(holder),
            epoch,
            ttl: Duration::from_secs(self.config.ttl_secs),
            expires_at_unix_ms: now + self.config.ttl_secs * 1000,
            renewed_at_unix_ms: now,
        };
        leases.insert(holder.to_string(), lease.clone());
        Ok(lease)
    }

    /// 心跳续租：token 必须匹配（旧 token 拒绝，防过期节点复活续租）。
    pub fn renew(&self, holder: &str, token: &str) -> Result<Lease, LeaseError> {
        let now = now_unix_ms();
        let mut leases = self
            .leases
            .lock()
            .map_err(|_| LeaseError::ReadOnly("锁异常".into()))?;
        let lease = leases
            .get_mut(holder)
            .ok_or_else(|| LeaseError::UnknownHolder(holder.to_string()))?;
        if lease.token != token {
            return Err(LeaseError::BadToken {
                expected: lease.token.clone(),
                got: token.to_string(),
            });
        }
        lease.expires_at_unix_ms = now + self.config.ttl_secs * 1000;
        lease.renewed_at_unix_ms = now;
        Ok(lease.clone())
    }

    /// fencing 写校验：持有者存在、token 匹配、未过期、纪元匹配、控制面非只读。
    pub fn verify_write(
        &self,
        holder: &str,
        token: &str,
        expected_epoch: u64,
    ) -> Result<(), LeaseError> {
        if self.is_read_only() {
            return Err(LeaseError::ReadOnly(holder.to_string()));
        }
        let now = now_unix_ms();
        let leases = self
            .leases
            .lock()
            .map_err(|_| LeaseError::ReadOnly("锁异常".into()))?;
        let lease = leases
            .get(holder)
            .ok_or_else(|| LeaseError::UnknownHolder(holder.to_string()))?;
        if lease.token != token {
            return Err(LeaseError::BadToken {
                expected: lease.token.clone(),
                got: token.to_string(),
            });
        }
        if lease.is_expired(now) {
            return Err(LeaseError::Expired(holder.to_string()));
        }
        if lease.epoch != expected_epoch {
            return Err(LeaseError::Fenced {
                holder: holder.to_string(),
                expected_epoch,
                got_epoch: lease.epoch,
            });
        }
        Ok(())
    }

    /// 释放租约（token 匹配才释放）。
    pub fn release(&self, holder: &str, token: &str) -> Result<(), LeaseError> {
        let mut leases = self
            .leases
            .lock()
            .map_err(|_| LeaseError::ReadOnly("锁异常".into()))?;
        match leases.get(holder) {
            Some(lease) if lease.token == token => {
                leases.remove(holder);
                Ok(())
            }
            Some(lease) => Err(LeaseError::BadToken {
                expected: lease.token.clone(),
                got: token.to_string(),
            }),
            None => Err(LeaseError::UnknownHolder(holder.to_string())),
        }
    }

    /// 查询租约（不含过期判定；`lease()` 供诊断）。
    pub fn lease(&self, holder: &str) -> Option<Lease> {
        self.leases.lock().ok().and_then(|l| l.get(holder).cloned())
    }

    pub fn holders(&self) -> Vec<String> {
        self.leases
            .lock()
            .map(|l| l.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// 清理过期租约（返回清理数；超时迁移/失败路径）。
    pub fn reap_expired(&self) -> usize {
        let now = now_unix_ms();
        let mut leases = self.leases.lock().unwrap();
        let before = leases.len();
        leases.retain(|_, l| !l.is_expired(now));
        before - leases.len()
    }
}

fn new_token(holder: &str) -> String {
    format!("tok-{holder}-{}", uuid::Uuid::new_v4())
}
