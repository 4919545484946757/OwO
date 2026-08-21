//! 本地 API 限流（R7 X03）：全局 + 每会话 + 敏感端点 双令牌桶。
//!
//! - 全局桶：`OWO_API_RPM_GLOBAL`（默认 600，突发=容量=RPM）；
//! - 每会话桶：`OWO_API_RPM_SESSION`（默认 120），键 = `/session/{id}/…` 的会话 id；
//! - 敏感端点桶：`OWO_API_RPM_SENSITIVE`（默认 20），覆盖 /command、/subagent、
//!   /team/import、/plugins/market/install-remote、/eval/gate/run、/computer-use/task、
//!   /cloud/tasks（写操作面）。
//! - 敏感优先于会话优先于全局：任一桶不足 → 429 + `Retry-After` 秒数 + 审计记录。
//! - 配置经环境变量，测试用显式 `RateLimitConfig`。
//!
//! 本模块不引用 `crate::`/`super::`（AppState 全限定），可被测试以
//! `#[path] mod` 独立编译。

// 与 team_api.rs 同款模块级 allow(dead_code)：lib 目标经 build_router 使用
// enforce_rate_limit；#[path] 独立编译的测试目标内中间件未被调用，
// 避免 clippy -D warnings 在测试目标误报。
#![allow(dead_code)]

use axum::extract::State;
use axum::http::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use owo_agent_server::AppState;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[cfg(test)]
use std::time::Duration;

/// 敏感端点前缀（写操作面收紧）。
pub const SENSITIVE_PREFIXES: &[&str] = &[
    "/command/",
    "/subagent/run",
    "/team/import",
    "/team/review",
    "/plugins/market/install-remote",
    "/eval/gate/run",
    "/computer-use/task/",
    "/cloud/tasks",
];

/// 令牌桶配置。
#[derive(Debug, Clone, Copy)]
pub struct BucketSpec {
    /// 容量（最大突发数）。
    pub capacity: f64,
    /// 每秒补充速率（RPM/60）。
    pub refill_per_sec: f64,
}

impl BucketSpec {
    pub fn rpm(rpm: u64) -> Self {
        Self {
            capacity: rpm as f64,
            refill_per_sec: rpm as f64 / 60.0,
        }
    }
}

/// 限流配置。
#[derive(Debug, Clone, Copy)]
pub struct RateLimitConfig {
    pub global: BucketSpec,
    pub session: BucketSpec,
    pub sensitive: BucketSpec,
}

impl RateLimitConfig {
    /// 从环境变量读取（缺省 600/120/20 RPM）。
    pub fn from_env() -> Self {
        Self {
            global: BucketSpec::rpm(env_u64("OWO_API_RPM_GLOBAL", 600)),
            session: BucketSpec::rpm(env_u64("OWO_API_RPM_SESSION", 120)),
            sensitive: BucketSpec::rpm(env_u64("OWO_API_RPM_SENSITIVE", 20)),
        }
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

/// 单个令牌桶（容量=突发上限；满桶时 tokens=capacity）。
#[derive(Debug, Clone)]
pub struct TokenBucket {
    spec: BucketSpec,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    pub fn new(spec: BucketSpec) -> Self {
        Self {
            spec,
            tokens: spec.capacity,
            last: Instant::now(),
        }
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.spec.refill_per_sec).min(self.spec.capacity);
    }

    /// 尝试消耗一个令牌；成功返回 true。
    pub fn try_take(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// 当前剩余令牌（测试用）。
    pub fn tokens(&mut self) -> f64 {
        self.refill();
        self.tokens
    }

    /// 到下一个令牌的预计秒数。
    pub fn retry_after_secs(&mut self) -> u64 {
        self.refill();
        if self.tokens >= 1.0 {
            0
        } else {
            ((1.0 - self.tokens) / self.spec.refill_per_sec).ceil() as u64
        }
    }
}

/// 限流器：全局桶 + 每会话桶注册表 + 敏感桶。
pub struct RateLimiter {
    cfg: RateLimitConfig,
    global: Mutex<TokenBucket>,
    sessions: Mutex<HashMap<String, Arc<Mutex<TokenBucket>>>>,
    sensitive: Mutex<TokenBucket>,
}

impl RateLimiter {
    pub fn new(cfg: RateLimitConfig) -> Self {
        Self {
            cfg,
            global: Mutex::new(TokenBucket::new(cfg.global)),
            sessions: Mutex::new(HashMap::new()),
            sensitive: Mutex::new(TokenBucket::new(cfg.sensitive)),
        }
    }

    pub fn from_env() -> Self {
        Self::new(RateLimitConfig::from_env())
    }

    /// 判断请求是否应放行。依次检查敏感/会话/全局桶。
    /// 返回 (是否放行, retry_after_secs)。
    pub fn allow(&self, method: &str, path: &str) -> (bool, u64) {
        if method == "GET" || method == "OPTIONS" {
            // 读面不做限流（静态/轮询/SSE 重连）；写面与混合方法走限流。
            if !is_sensitive_get(method, path) {
                return (true, 0);
            }
        }
        let mut sensitive = self.sensitive.lock().unwrap_or_else(|e| e.into_inner());
        if is_sensitive_path(path) {
            let ok = sensitive.try_take();
            let retry = if ok { 0 } else { sensitive.retry_after_secs() };
            return (ok, retry);
        }
        drop(sensitive);

        if let Some(session_id) = session_id_from_path(path) {
            let bucket = {
                let mut map = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
                map.entry(session_id.to_string())
                    .or_insert_with(|| Arc::new(Mutex::new(TokenBucket::new(self.cfg.session))))
                    .clone()
            };
            let mut bucket = bucket.lock().unwrap_or_else(|e| e.into_inner());
            let ok = bucket.try_take();
            let retry = if ok { 0 } else { bucket.retry_after_secs() };
            return (ok, retry);
        }

        let mut global = self.global.lock().unwrap_or_else(|e| e.into_inner());
        let ok = global.try_take();
        let retry = if ok { 0 } else { global.retry_after_secs() };
        (ok, retry)
    }

    /// 会话桶数量（测试用）。
    pub fn session_count(&self) -> usize {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }
}

/// 路径是否属于敏感面。
pub fn is_sensitive_path(path: &str) -> bool {
    SENSITIVE_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

/// GET 也限流的敏感端点（如 /eval/gate/report 高频拉取可单列；当前仅 POST 面）。
fn is_sensitive_get(_method: &str, _path: &str) -> bool {
    false
}

/// 从 `/session/{id}/…` 提取会话 id。
pub fn session_id_from_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/session/")?;
    let id = rest.split('/').next().unwrap_or("");
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

/// 公开端点/SSE 路径不参与限流（与 auth_token 的豁免集合一致）。
pub fn exempt_path(path: &str) -> bool {
    matches!(path, "/health" | "/openapi.json" | "/auth/token")
        || path.ends_with("/events")
        || path == "/events/stream"
}

/// 限流中间件：429 + Retry-After + 审计。
pub async fn enforce_rate_limit(
    State(state): State<Arc<AppState>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    if exempt_path(&path) {
        return next.run(request).await;
    }
    let method = request.method().as_str().to_string();
    let (allowed, retry_after) = state.rate_limiter.allow(&method, &path);
    if !allowed {
        if let Ok(mut audit) = state.agent.audit_log().lock() {
            audit.record(
                "rate_limit",
                "rate_limited",
                Some(path.clone()),
                Some(false),
                format!("限流拒绝（{method} {path}）"),
            );
        }
        let response = (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            [(
                axum::http::header::RETRY_AFTER,
                retry_after.max(1).to_string(),
            )],
            Json(json!({
                "error": "请求过于频繁，请稍后重试",
                "code": "gateway/rate_limited/retryable",
                "retry_after_secs": retry_after.max(1),
            })),
        )
            .into_response();
        return response;
    }
    next.run(request).await
}

/// AppState 内配置快照（供测试断言实际生效的桶参数）。
impl RateLimiter {
    pub fn config(&self) -> RateLimitConfig {
        self.cfg
    }
}

// ---------- 单元测试（独立编译） ----------

#[cfg(test)]
mod tests {
    use super::*;

    fn small_cfg(rpm: u64) -> RateLimitConfig {
        RateLimitConfig {
            global: BucketSpec::rpm(rpm),
            session: BucketSpec::rpm(rpm),
            sensitive: BucketSpec::rpm(rpm),
        }
    }

    #[test]
    fn bucket_allows_until_capacity() {
        let mut bucket = TokenBucket::new(BucketSpec::rpm(5));
        assert!(bucket.tokens() > 4.9);
        for _ in 0..5 {
            assert!(bucket.try_take());
        }
        assert!(!bucket.try_take());
    }

    #[test]
    fn bucket_refills_over_time() {
        let mut bucket = TokenBucket::new(BucketSpec {
            capacity: 1.0,
            refill_per_sec: 100.0,
        });
        assert!(bucket.try_take());
        assert!(!bucket.try_take());
        std::thread::sleep(Duration::from_millis(30));
        assert!(bucket.try_take(), "30ms 后应补充令牌");
    }

    #[test]
    fn retry_after_is_positive_when_empty() {
        let mut bucket = TokenBucket::new(BucketSpec {
            capacity: 1.0,
            refill_per_sec: 0.1,
        });
        assert!(bucket.try_take());
        assert!(!bucket.try_take());
        assert!(bucket.retry_after_secs() >= 1);
    }

    #[test]
    fn global_bucket_rejects_over_rpm() {
        let limiter = RateLimiter::new(small_cfg(5));
        let mut allowed = 0;
        for _ in 0..10 {
            if limiter.allow("POST", "/goal").0 {
                allowed += 1;
            }
        }
        assert_eq!(allowed, 5);
    }

    #[test]
    fn session_buckets_are_independent() {
        let limiter = RateLimiter::new(small_cfg(3));
        for _ in 0..3 {
            assert!(limiter.allow("POST", "/session/a/turn").0);
        }
        assert!(!limiter.allow("POST", "/session/a/turn").0);
        // 另一会话不受影响。
        assert!(limiter.allow("POST", "/session/b/turn").0);
        assert_eq!(limiter.session_count(), 2);
    }

    #[test]
    fn sensitive_bucket_is_stricter() {
        let cfg = RateLimitConfig {
            global: BucketSpec::rpm(100),
            session: BucketSpec::rpm(100),
            sensitive: BucketSpec::rpm(3),
        };
        let limiter = RateLimiter::new(cfg);
        for _ in 0..3 {
            assert!(limiter.allow("POST", "/command/run").0);
        }
        let (ok, retry) = limiter.allow("POST", "/command/run");
        assert!(!ok);
        assert!(retry >= 1);
    }

    #[test]
    fn session_id_extracted_from_session_paths() {
        assert_eq!(
            session_id_from_path("/session/abc-123/turn").as_deref(),
            Some("abc-123")
        );
        assert_eq!(
            session_id_from_path("/session/abc-123").as_deref(),
            Some("abc-123")
        );
        assert_eq!(session_id_from_path("/goal/x/run"), None);
        assert_eq!(session_id_from_path("/session//turn"), None);
    }

    #[test]
    fn reads_are_not_rate_limited() {
        let limiter = RateLimiter::new(small_cfg(2));
        for _ in 0..50 {
            assert!(limiter.allow("GET", "/skills").0, "GET 不消耗桶");
        }
    }

    #[test]
    fn config_from_env_defaults() {
        let cfg = RateLimitConfig::from_env();
        assert!(cfg.global.capacity >= 600.0);
        assert!(cfg.session.capacity >= 120.0);
        assert!(cfg.sensitive.capacity >= 20.0);
    }
}
