//! 用量与预算硬熔断契约测试（R12 Agent 4）：预算超限即时停轮 / 402 响应 / 加额恢复 / 四维报表 / 持久化恢复。
//!
//! 独立编译：`#[path = "../src/usage.rs"] mod usage;`（usage.rs 不引用 crate::/super::）。

use std::path::Path;

#[path = "../src/usage.rs"]
mod usage;

use usage::{UsageDimension, UsageStore};

#[test]
fn budget_exceeded_sets_hard_stop_immediately() {
    let store = UsageStore::new();
    store.set_budget(UsageDimension::Session, 0.0); // 零预算
    assert!(!store.is_hard_stopped());
    // 单次记录即触发硬熔断（R11：累计后立即 recheck，不再等下轮 check_budget）。
    store.record_tokens(UsageDimension::Session, "s1", None, 1_000_000, 0);
    assert!(store.is_hard_stopped(), "预算超限当次必须置位硬熔断");
    assert!(store.hard_stop_reason().is_some());
    assert!(store.check_budget(), "check_budget 应反映熔断");
}

#[test]
fn budget_exceeded_response_is_402_with_code() {
    let (status, body) = usage::budget_exceeded_response("session 预算超限");
    assert_eq!(status.as_u16(), 402);
    assert_eq!(body.0["code"], serde_json::json!("budget_exceeded"));
    assert_eq!(body.0["hard_stop"], serde_json::json!(true));
}

#[test]
fn topup_recovers_hard_stop() {
    let store = UsageStore::new();
    store.set_budget(UsageDimension::GoalStep, 0.000001);
    store.record_tokens(UsageDimension::GoalStep, "g1", None, 1000, 0);
    assert!(store.is_hard_stopped());
    // 加额后恢复：预算提高且熔断清除。
    store.request_topup(UsageDimension::GoalStep, 100.0);
    assert!(!store.is_hard_stopped(), "加额后必须恢复（可继续运行）");
    assert!(store.hard_stop_reason().is_none());
}

#[test]
fn summary_aggregates_four_dimensions_and_budget_state() {
    usage::reset_global_for_test();
    let store = usage::global();
    store.set_budget(UsageDimension::Session, 10.0);
    store.set_budget(UsageDimension::Tool, 10.0);
    store.record_tokens(UsageDimension::Session, "s1", None, 100, 50);
    store.record_tokens(UsageDimension::WorkflowRun, "w1", None, 200, 100);
    store.record_tokens(UsageDimension::GoalStep, "g1", None, 300, 150);
    store.record_tokens(UsageDimension::Tool, "read_file", None, 400, 200);
    let summary = usage::global().summary();
    let dims = summary["dimensions"].as_array().unwrap();
    assert_eq!(dims.len(), 4, "四维报表应含 4 个维度");
    let session = dims
        .iter()
        .find(|d| d["dimension"] == "session")
        .expect("含 session 维度");
    assert_eq!(session["calls"], serde_json::json!(1));
    assert_eq!(session["total_tokens"], serde_json::json!(150));
    assert!(session["cost_usd"].as_f64().unwrap() > 0.0);
    assert_eq!(session["budget"]["limit_usd"], serde_json::json!(10.0));
    assert_eq!(summary["hard_stop"], serde_json::json!(false));
    usage::reset_global_for_test();
}

#[test]
fn persist_load_restores_records_budgets_and_hard_stop() {
    usage::reset_global_for_test();
    let dir = std::env::temp_dir().join(format!("owo-usage-t-{}", uuid::Uuid::new_v4()));
    let store = usage::global();
    store.set_budget(UsageDimension::Session, 0.000001);
    store.record_tokens(UsageDimension::Session, "s1", None, 1000, 0);
    assert!(store.is_hard_stopped());
    // 落盘 → 重置 → 恢复：记录/预算/硬熔断都复原。
    usage::persist_to(&dir).unwrap();
    usage::reset_global_for_test();
    let restored = usage::load_from(&dir).unwrap();
    assert_eq!(restored, 1, "应恢复 1 条记录");
    let summary = usage::global().summary();
    assert_eq!(summary["count"], serde_json::json!(1));
    assert_eq!(
        summary["hard_stop"],
        serde_json::json!(true),
        "硬熔断必须随快照恢复"
    );
    assert!(summary["hard_stop_reason"].is_string());
    usage::reset_global_for_test();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn record_usage_ring_buffer_caps_at_limit() {
    let store = UsageStore::new();
    for i in 0..(usage::USAGE_RECORDS_CAP + 50) {
        store.record_tokens(UsageDimension::Tool, &format!("t{i}"), None, 1, 0);
    }
    assert_eq!(store.records().len(), usage::USAGE_RECORDS_CAP);
}
