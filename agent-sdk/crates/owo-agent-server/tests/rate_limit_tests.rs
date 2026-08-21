//! rate_limit 契约测试（R7 X03）：令牌桶/全局/每会话/敏感端点/Retry-After。
//!
//! 独立编译目标：`rate_limit.rs` 不引用 crate::/super::，本文件用 #[path] 挂载。

#[path = "../src/rate_limit.rs"]
mod rate_limit;

use rate_limit::{BucketSpec, RateLimitConfig, RateLimiter};
use std::time::Duration;

fn small_cfg(rpm: u64) -> RateLimitConfig {
    RateLimitConfig {
        global: BucketSpec::rpm(rpm),
        session: BucketSpec::rpm(rpm),
        sensitive: BucketSpec::rpm(rpm),
    }
}

#[test]
fn bucket_allows_up_to_capacity_then_rejects() {
    let mut bucket = rate_limit::TokenBucket::new(BucketSpec::rpm(5));
    for _ in 0..5 {
        assert!(bucket.try_take());
    }
    assert!(!bucket.try_take(), "容量耗尽应拒绝");
}

#[test]
fn bucket_refills_after_time() {
    let mut bucket = rate_limit::TokenBucket::new(BucketSpec {
        capacity: 1.0,
        refill_per_sec: 200.0,
    });
    assert!(bucket.try_take());
    assert!(!bucket.try_take());
    std::thread::sleep(Duration::from_millis(30));
    assert!(bucket.try_take(), "30ms 后应补充令牌");
}

#[test]
fn retry_after_reported_when_empty() {
    let mut bucket = rate_limit::TokenBucket::new(BucketSpec {
        capacity: 1.0,
        refill_per_sec: 0.2,
    });
    assert!(bucket.try_take());
    assert!(!bucket.try_take());
    let wait = bucket.retry_after_secs();
    assert!(wait >= 1, "空桶 retry_after 应 ≥1s，实际 {wait}");
}

#[test]
fn global_bucket_limits_total_requests() {
    let limiter = RateLimiter::new(small_cfg(4));
    let mut allowed = 0;
    for _ in 0..10 {
        if limiter.allow("POST", "/goal").0 {
            allowed += 1;
        }
    }
    assert_eq!(allowed, 4);
}

#[test]
fn session_buckets_are_independent() {
    let limiter = RateLimiter::new(small_cfg(2));
    assert!(limiter.allow("POST", "/session/a/turn").0);
    assert!(limiter.allow("POST", "/session/a/turn").0);
    assert!(!limiter.allow("POST", "/session/a/turn").0);
    assert!(
        limiter.allow("POST", "/session/b/turn").0,
        "会话 b 不受 a 影响"
    );
    assert!(limiter.session_count() >= 2);
}

#[test]
fn sensitive_endpoints_use_stricter_bucket() {
    let cfg = RateLimitConfig {
        global: BucketSpec::rpm(100),
        session: BucketSpec::rpm(100),
        sensitive: BucketSpec::rpm(2),
    };
    // 敏感桶全局共享：每个路径用独立限流器验证（避免跨路径消耗）。
    for path in [
        "/command/run",
        "/team/import",
        "/plugins/market/install-remote",
        "/eval/gate/run",
        "/cloud/tasks",
    ] {
        let limiter = RateLimiter::new(cfg);
        let mut ok_count = 0;
        for _ in 0..4 {
            if limiter.allow("POST", path).0 {
                ok_count += 1;
            }
        }
        assert_eq!(ok_count, 2, "{path} 应受敏感桶 2 RPM 约束");
    }
}

#[test]
fn sensitive_rejection_has_retry_after() {
    let cfg = RateLimitConfig {
        global: BucketSpec::rpm(100),
        session: BucketSpec::rpm(100),
        sensitive: BucketSpec::rpm(1),
    };
    let limiter = RateLimiter::new(cfg);
    assert!(limiter.allow("POST", "/command/run").0);
    let (ok, retry) = limiter.allow("POST", "/command/run");
    assert!(!ok);
    assert!(retry >= 1);
}

#[test]
fn read_methods_do_not_consume_tokens() {
    let limiter = RateLimiter::new(small_cfg(2));
    for _ in 0..50 {
        assert!(limiter.allow("GET", "/skills").0);
        assert!(limiter.allow("GET", "/session/s1/diff").0);
    }
}

#[test]
fn session_id_extraction_rules() {
    assert_eq!(
        rate_limit::session_id_from_path("/session/abc/turn").as_deref(),
        Some("abc")
    );
    assert_eq!(
        rate_limit::session_id_from_path("/session/x").as_deref(),
        Some("x")
    );
    assert_eq!(rate_limit::session_id_from_path("/sessions"), None);
    assert_eq!(rate_limit::session_id_from_path("/goal/g/run"), None);
    assert_eq!(rate_limit::session_id_from_path("/session//turn"), None);
}

#[test]
fn env_config_has_sane_defaults() {
    let cfg = RateLimitConfig::from_env();
    assert!(cfg.global.capacity >= 600.0);
    assert!(cfg.session.capacity >= 120.0);
    assert!(cfg.sensitive.capacity >= 20.0);
    let limiter = RateLimiter::from_env();
    assert!(limiter.config().global.capacity >= 600.0);
}

#[test]
fn exempt_paths_skip_rate_limiting() {
    assert!(rate_limit::exempt_path("/health"));
    assert!(rate_limit::exempt_path("/openapi.json"));
    assert!(rate_limit::exempt_path("/auth/token"));
    assert!(rate_limit::exempt_path("/cloud/tasks/x/events"));
    assert!(!rate_limit::exempt_path("/goal"));
    assert!(!rate_limit::exempt_path("/command/run"));
}

#[test]
fn limiter_survives_concurrent_access() {
    let limiter = std::sync::Arc::new(RateLimiter::new(small_cfg(1000)));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let limiter = std::sync::Arc::clone(&limiter);
        handles.push(std::thread::spawn(move || {
            for _ in 0..50 {
                let _ = limiter.allow("POST", &format!("/session/{}", uuid::Uuid::new_v4()));
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    assert!(limiter.session_count() > 0);
}
