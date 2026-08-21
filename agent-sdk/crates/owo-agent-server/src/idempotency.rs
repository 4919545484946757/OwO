//! 幂等与去重（R6 Agent 4 Wave 1）：`IdempotencyRegistry`。
//!
//! - 幂等键注册表 + 响应缓存：重复提交返回首次执行的结果（零重复写）。
//! - 与 `correlation_id` 关联：`key(correlation_id, operation)` 合成复合键，
//!   支持 at-least-once 投递下的端到端去重。
//! - `execute` 在注册表锁内完成查→执行→缓存（串行化），并发重复提交时
//!   executor 也至多执行一次；代价是同一注册表上的执行互斥，契约正确性优先。
//! - 有界缓存（默认 10_000 条）+ TTL（默认 24h）：插入满时逐出最旧条目。
//! - `writes()`/`hits()` 计数器暴露实际执行/命中次数，供可观测性度量去重效果。
//! - 本模块不引用 `crate::`/`super::`，可被测试以 `#[path] mod` 独立编译。

// 主控收尾接线说明：lib 目标当前仅登记模块（无路由引用），全部符号由
// idempotency_tests 以 #[path] 独立编译使用；与 team_api.rs 同款模块级
// allow(dead_code)（后续接入幂等端点后移除）。
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 缓存默认上限（条数）。
pub const DEFAULT_MAX_ENTRIES: usize = 10_000;
/// 缓存默认 TTL。
pub const DEFAULT_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// 缓存的响应快照（原始响应原样返回，含重试语义）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedResponse {
    pub status: u16,
    pub body: Value,
    pub retry_after_ms: Option<u64>,
    pub correlation_id: Option<String>,
}

struct RegistryState {
    order: VecDeque<String>,
    index: HashMap<String, (CachedResponse, u64)>,
}

/// 幂等注册表：`key → CachedResponse`（插入序 VecDeque + 哈希索引 O(1) 查）。
#[derive(Clone)]
pub struct IdempotencyRegistry {
    inner: Arc<RegistryInner>,
}

struct RegistryInner {
    entries: Mutex<RegistryState>,
    max_entries: usize,
    ttl: Duration,
    writes: AtomicU64,
    hits: AtomicU64,
}

impl IdempotencyRegistry {
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_MAX_ENTRIES, DEFAULT_TTL)
    }

    pub fn with_limits(max_entries: usize, ttl: Duration) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                entries: Mutex::new(RegistryState {
                    order: VecDeque::new(),
                    index: HashMap::new(),
                }),
                max_entries,
                ttl,
                writes: AtomicU64::new(0),
                hits: AtomicU64::new(0),
            }),
        }
    }

    /// 合成幂等键：`{correlation_id}:{operation}`（correlation_id 为空时仅 operation）。
    pub fn key(correlation_id: Option<&str>, operation: &str) -> String {
        match correlation_id {
            Some(id) if !id.is_empty() => format!("{id}:{operation}"),
            _ => operation.to_string(),
        }
    }

    /// 查询缓存命中（TTL 过期条目被清理并视为未命中）。
    pub fn get(&self, key: &str) -> Option<CachedResponse> {
        let mut state = self.inner.entries.lock().unwrap_or_else(|e| e.into_inner());
        let hit = self.fetch_or_clean(&mut state, key, now_ms());
        if hit.is_some() {
            self.inner.hits.fetch_add(1, Ordering::Relaxed);
        }
        hit
    }

    /// 幂等执行：在锁内完成 查→执行→缓存；重复提交（含并发）executor 至多一次。
    pub fn execute(
        &self,
        key: &str,
        correlation_id: Option<&str>,
        executor: impl FnOnce() -> CachedResponse,
    ) -> CachedResponse {
        let mut state = self.inner.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = self.fetch_or_clean(&mut state, key, now_ms()) {
            self.inner.hits.fetch_add(1, Ordering::Relaxed);
            return cached;
        }
        let mut response = executor();
        if response.correlation_id.is_none() {
            response.correlation_id = correlation_id.map(str::to_string);
        }
        self.store(&mut state, key, response.clone(), now_ms());
        self.inner.writes.fetch_add(1, Ordering::Relaxed);
        response
    }

    /// 写入缓存（逐出最旧 + TTL 清理）。
    pub fn insert(&self, key: &str, response: CachedResponse) {
        let mut state = self.inner.entries.lock().unwrap_or_else(|e| e.into_inner());
        self.store(&mut state, key, response, now_ms());
    }

    /// 实际执行次数（去重效果度量）。
    pub fn writes(&self) -> u64 {
        self.inner.writes.load(Ordering::Relaxed)
    }

    /// 缓存命中次数。
    pub fn hits(&self) -> u64 {
        self.inner.hits.load(Ordering::Relaxed)
    }

    /// 当前缓存条目数。
    pub fn len(&self) -> usize {
        self.inner
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .order
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn fetch_or_clean(
        &self,
        state: &mut RegistryState,
        key: &str,
        now: u64,
    ) -> Option<CachedResponse> {
        match state.index.get(key) {
            Some((response, created_at))
                if now.saturating_sub(*created_at) < self.inner.ttl.as_millis() as u64 =>
            {
                Some(response.clone())
            }
            Some(_) => {
                state.index.remove(key);
                state.order.retain(|k| k != key);
                None
            }
            None => None,
        }
    }

    fn store(&self, state: &mut RegistryState, key: &str, response: CachedResponse, now: u64) {
        let max = self.inner.max_entries;
        let ttl_ms = self.inner.ttl.as_millis() as u64;
        state.index.insert(key.to_string(), (response, now));
        state.order.retain(|k| k != key);
        state.order.push_back(key.to_string());
        // TTL 清理：从最旧开始。
        state.order.retain(|k| {
            state
                .index
                .get(k)
                .map(|(_, created)| now.saturating_sub(*created) < ttl_ms)
                .unwrap_or(false)
        });
        while state.order.len() > max {
            if let Some(oldest) = state.order.pop_front() {
                state.index.remove(&oldest);
            }
        }
    }
}

impl Default for IdempotencyRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
