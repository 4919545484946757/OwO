//! 分层错误码契约测试（R6 Agent 4 Wave 1）：域/原因/可恢复性 + HTTP 映射。
//!
//! 独立编译：`#[path = "../src/error_codes.rs"] mod error_codes;`。
//! 覆盖：三段式解析、注册表 HTTP 映射、retryable 分类、retry_after 语义、
//! 未知原因兜底、非法格式拒绝、JSON 序列化形态、快捷构造。

use std::time::Duration;

#[path = "../src/error_codes.rs"]
mod error_codes;

use error_codes::ErrorCode;

#[test]
fn parse_layered_code_roundtrip() {
    let code = ErrorCode::from_code("gateway/rate_limited/retryable").unwrap();
    assert_eq!(code.domain, "gateway");
    assert_eq!(code.reason, "rate_limited");
    assert!(code.retryable);
    assert_eq!(code.http_status(), 429);

    let not_retryable = ErrorCode::from_code("permission/denied/not_retryable").unwrap();
    assert!(!not_retryable.retryable);
    assert_eq!(not_retryable.http_status(), 403);
}

#[test]
fn known_codes_map_to_http_statuses() {
    let cases: Vec<(&str, u16)> = vec![
        ("gateway/rate_limited/retryable", 429),
        ("gateway/unavailable/retryable", 503),
        ("gateway/timeout/retryable", 504),
        ("gateway/circuit_open/retryable", 503),
        ("permission/denied/not_retryable", 403),
        ("auth/missing_credentials/not_retryable", 401),
        ("validation/invalid_input/not_retryable", 400),
        ("validation/conflict/not_retryable", 409),
        ("storage/not_found/not_retryable", 404),
        ("tool/not_found/not_retryable", 404),
        ("tool/execution_failed/retryable", 502),
        ("internal/unexpected/retryable", 500),
    ];
    for (code_str, expected_status) in cases {
        let code = ErrorCode::from_code(code_str).unwrap();
        assert_eq!(
            code.http_status(),
            expected_status,
            "错误码 {code_str} 应映射到 HTTP {expected_status}"
        );
    }
}

#[test]
fn retryable_flag_drives_retry_after_semantics() {
    let retryable = ErrorCode::from_code("gateway/rate_limited/retryable").unwrap();
    assert_eq!(retryable.retry_after(), Some(Duration::from_millis(1_000)));
    let unavailable = ErrorCode::from_code("gateway/unavailable/retryable").unwrap();
    assert_eq!(
        unavailable.retry_after(),
        Some(Duration::from_millis(5_000))
    );
    // 显式标记覆盖注册表默认：标记 not_retryable 即使注册表带重试时间也不返回。
    let forced = ErrorCode::from_code("gateway/rate_limited/not_retryable").unwrap();
    assert!(!forced.retryable);
    assert_eq!(forced.retry_after(), None);
    // 非可恢复错误无 retry_after。
    let denied = ErrorCode::from_code("permission/denied/not_retryable").unwrap();
    assert_eq!(denied.retry_after(), None);
}

#[test]
fn unknown_reason_falls_back_to_reason_mapping() {
    let circuit = ErrorCode::from_code("gateway/circuit_open/retryable").unwrap();
    assert_eq!(circuit.http_status(), 503);
    let weird = ErrorCode::from_code("some_domain/weird_reason/retryable").unwrap();
    assert_eq!(weird.http_status(), 500);
    assert_eq!(weird.retry_after(), Some(Duration::from_millis(5_000)));
    let weird_not = ErrorCode::from_code("some_domain/weird_reason/not_retryable").unwrap();
    assert_eq!(weird_not.http_status(), 500);
    assert_eq!(weird_not.retry_after(), None);
}

#[test]
fn malformed_codes_rejected() {
    for bad in [
        "",
        "gateway",
        "gateway/rate_limited",
        "gateway/rate_limited/retryable/extra",
        "gateway//retryable",
        "/rate_limited/retryable",
        "gateway/rate_limited/maybe",
    ] {
        assert!(
            ErrorCode::from_code(bad).is_err(),
            "非法错误码应被拒绝：{bad:?}"
        );
    }
}

#[test]
fn to_json_serializes_full_contract_shape() {
    let code = ErrorCode::from_code("gateway/rate_limited/retryable").unwrap();
    let json = code.to_json("too many requests");
    assert_eq!(json["code"], "gateway/rate_limited/retryable");
    assert_eq!(json["domain"], "gateway");
    assert_eq!(json["reason"], "rate_limited");
    assert_eq!(json["retryable"], serde_json::json!(true));
    assert_eq!(json["http_status"], serde_json::json!(429));
    assert_eq!(json["retry_after_ms"], serde_json::json!(1_000));
    assert_eq!(json["message"], "too many requests");

    let denied = ErrorCode::from_code("permission/denied/not_retryable").unwrap();
    let denied_json = denied.to_json("blocked");
    assert_eq!(denied_json["retryable"], serde_json::json!(false));
    assert_eq!(denied_json["retry_after_ms"], serde_json::Value::Null);
}

#[test]
fn lookup_table_is_stable() {
    // 注册表查（域, 原因）与解析一致。
    let from_lookup = ErrorCode::lookup("validation", "invalid_input").unwrap();
    let from_parse = ErrorCode::from_code("validation/invalid_input/not_retryable").unwrap();
    assert_eq!(from_lookup.http_status, from_parse.http_status);
    assert_eq!(from_lookup.domain, from_parse.domain);
    // 未登记组合返回 None。
    assert!(ErrorCode::lookup("nope", "nope").is_none());
}

#[test]
fn code_constructor_is_total() {
    let code = error_codes::code("gateway", "rate_limited", true);
    assert_eq!(code.http_status(), 429);
    let code = error_codes::code("any", "anything", false);
    assert_eq!(code.http_status(), 500);
    assert_eq!(code.retry_after(), None);
}
