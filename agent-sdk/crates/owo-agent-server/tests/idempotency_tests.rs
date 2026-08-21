//! 幂等与去重契约测试（R6 Agent 4 Wave 1）：重复提交零重复写。
//!
//! 独立编译：`#[path = "../src/idempotency.rs"] mod idempotency;`。
//! 覆盖：缓存命中返回原结果、executor 至多执行一次、correlation_id 复合键、
//! TTL 过期、上限逐出、响应字段保留、写/命中计数。

use serde_json::json;
use std::time::Duration;

#[path = "../src/idempotency.rs"]
mod idempotency;

use idempotency::{CachedResponse, IdempotencyRegistry};

fn response(status: u16, body: serde_json::Value) -> CachedResponse {
    CachedResponse {
        status,
        body,
        retry_after_ms: None,
        correlation_id: None,
    }
}

#[test]
fn duplicate_submission_returns_cached_executor_runs_once() {
    let registry = IdempotencyRegistry::new();
    let mut writes = 0usize;
    let first = registry.execute("op:submit:1", Some("corr-1"), || {
        writes += 1;
        response(201, json!({ "id": "job-1" }))
    });
    let second = registry.execute("op:submit:1", Some("corr-1"), || {
        writes += 1;
        response(201, json!({ "id": "job-1" }))
    });
    assert_eq!(first.body, second.body, "重复提交返回原结果");
    assert_eq!(first.status, 201);
    assert_eq!(writes, 1, "零重复写：executor 仅执行一次");
    assert_eq!(registry.writes(), 1);
    assert_eq!(registry.hits(), 1);
    assert!(!registry.is_empty(), "缓存已写入");
}

#[test]
fn correlation_key_scopes_operations_independently() {
    let registry = IdempotencyRegistry::new();
    let mut calls = 0u64;
    let key_a = IdempotencyRegistry::key(Some("corr-a"), "run_workflow");
    let key_b = IdempotencyRegistry::key(Some("corr-b"), "run_workflow");
    assert_ne!(key_a, key_b);
    registry.execute(&key_a, Some("corr-a"), || {
        calls += 1;
        response(200, json!({ "r": "a" }))
    });
    registry.execute(&key_b, Some("corr-b"), || {
        calls += 1;
        response(200, json!({ "r": "b" }))
    });
    // 同一 correlation_id 的同一操作去重；不同 id 互不影响。
    let dup = registry.execute(&key_a, Some("corr-a"), || {
        calls += 1;
        response(200, json!({ "r": "a" }))
    });
    assert_eq!(dup.body, json!({ "r": "a" }));
    assert_eq!(calls, 2, "同 id 去重，异 id 独立");
}

#[test]
fn key_without_correlation_falls_back_to_operation() {
    assert_eq!(IdempotencyRegistry::key(None, "approve"), "approve");
    assert_eq!(
        IdempotencyRegistry::key(Some(""), "approve"),
        "approve",
        "空 correlation_id 不参与合成"
    );
    assert_eq!(
        IdempotencyRegistry::key(Some("c-9"), "approve"),
        "c-9:approve"
    );
}

#[test]
fn ttl_expiry_re_executes_executor() {
    let registry = IdempotencyRegistry::with_limits(16, Duration::from_millis(5));
    let mut writes = 0u64;
    registry.execute("k", Some("c"), || {
        writes += 1;
        response(200, json!({ "n": 1 }))
    });
    std::thread::sleep(Duration::from_millis(30));
    let again = registry.execute("k", Some("c"), || {
        writes += 1;
        response(200, json!({ "n": 2 }))
    });
    assert_eq!(again.body, json!({ "n": 2 }), "TTL 过期后重新执行");
    assert_eq!(writes, 2);
    assert_eq!(registry.hits(), 0);
}

#[test]
fn max_entries_evicts_oldest() {
    let registry = IdempotencyRegistry::with_limits(3, Duration::from_secs(3600));
    for i in 0..5 {
        registry.insert(&format!("key-{i}"), response(200, json!({ "i": i })));
    }
    assert_eq!(registry.len(), 3);
    assert!(registry.get("key-0").is_none(), "最旧条目被逐出");
    assert!(registry.get("key-1").is_none());
    assert!(registry.get("key-2").is_some());
    assert!(registry.get("key-4").is_some());
    assert_eq!(registry.writes(), 0, "insert 不计数写入");
}

#[test]
fn response_fields_preserved_including_retry_after() {
    let registry = IdempotencyRegistry::new();
    let original = CachedResponse {
        status: 429,
        body: json!({ "code": "gateway/rate_limited/retryable" }),
        retry_after_ms: Some(1500),
        correlation_id: None,
    };
    // execute 时自动补记 correlation_id。
    let executed = registry.execute("op:rate", Some("corr-x"), || original.clone());
    assert_eq!(executed.correlation_id.as_deref(), Some("corr-x"));
    let cached = registry.get("op:rate").unwrap();
    assert_eq!(cached.status, 429);
    assert_eq!(cached.retry_after_ms, Some(1500));
    assert_eq!(cached.correlation_id.as_deref(), Some("corr-x"));
    assert_eq!(cached.body["code"], "gateway/rate_limited/retryable");
    // 再次 execute：命中缓存返回原结果，executor 不再执行。
    let dup = registry.execute("op:rate", Some("corr-x"), || {
        panic!("缓存命中时不应再执行 executor")
    });
    assert_eq!(dup.status, 429);
    assert_eq!(dup.body["code"], "gateway/rate_limited/retryable");
}

#[test]
fn concurrent_duplicate_single_execution() {
    let registry = IdempotencyRegistry::new();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let registry = registry.clone();
        let calls = std::sync::Arc::clone(&calls);
        handles.push(std::thread::spawn(move || {
            registry.execute("op:concurrent", Some("corr-c"), || {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                response(200, json!({ "ok": true }))
            })
        }));
    }
    for handle in handles {
        assert_eq!(handle.join().unwrap().status, 200);
    }
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "并发重复提交下 executor 仍只执行一次"
    );
}
