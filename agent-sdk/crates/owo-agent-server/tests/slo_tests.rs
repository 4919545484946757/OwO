//! SLO 注册表契约测试（R7 Agent 4 Wave 2）：check_slo/error_budget/report。
//!
//! 独立编译：`#[path = "../src/slo.rs"] mod slo;`。
//! 覆盖：默认基线、延迟型/成功率型判定、错误预算计算、p95、JSON 报告、
//! 全局注册表便捷路径与测试隔离。

#[path = "../src/slo.rs"]
mod slo;

use serde_json::Value;

#[test]
fn default_registry_contains_five_slo_baselines() {
    let registry = slo::SloRegistry::new();
    let names = registry.names();
    assert_eq!(names.len(), 5);
    for expected in [
        "audit_zero_loss",
        "http_success",
        "ipc",
        "panel_wake",
        "tool_schedule",
    ] {
        assert!(names.contains(&expected.to_string()), "缺 SLO：{expected}");
    }
    let ipc = registry.get("ipc").unwrap();
    assert_eq!(ipc.slo.target_ms, Some(5));
    let tool = registry.get("tool_schedule").unwrap();
    assert_eq!(tool.slo.target_ms, Some(10));
    let panel = registry.get("panel_wake").unwrap();
    assert_eq!(panel.slo.target_ms, Some(150));
    let audit = registry.get("audit_zero_loss").unwrap();
    assert_eq!(audit.slo.success_floor, Some(1.0));
    let http = registry.get("http_success").unwrap();
    assert_eq!(http.slo.success_floor, Some(0.999));
}

#[test]
fn check_slo_latency_within_target_is_ok() {
    let registry = slo::SloRegistry::new();
    let within = slo::check_slo(&registry, "ipc", Some(4), true);
    assert!(within, "IPC 4ms < 5ms 应达标");
    let within = slo::check_slo(&registry, "ipc", Some(5), true);
    assert!(within, "等于目标应达标（≤）");
}

#[test]
fn check_slo_latency_over_target_flags_violation() {
    let registry = slo::SloRegistry::new();
    let within = slo::check_slo(&registry, "tool_schedule", Some(42), true);
    assert!(!within, "42ms > 10ms 应违规");
    let state = registry.get("tool_schedule").unwrap();
    assert_eq!(state.violation_count(), 1);
}

#[test]
fn check_slo_ok_false_is_violation() {
    let registry = slo::SloRegistry::new();
    let within = slo::check_slo(&registry, "audit_zero_loss", None, false);
    assert!(!within, "审计丢失一次即违规");
    let within = slo::check_slo(&registry, "audit_zero_loss", None, true);
    assert!(within, "审计零丢失样本达标");
}

#[test]
fn check_slo_unknown_name_passes_through() {
    let registry = slo::SloRegistry::new();
    let within = slo::check_slo(&registry, "no_such_slo", Some(999), false);
    assert!(within, "未注册 SLO 名称宽松通过，不 panic");
}

#[test]
fn error_budget_healthy_within() {
    let registry = slo::SloRegistry::new();
    for _ in 0..1000 {
        slo::check_slo(&registry, "http_success", None, true);
    }
    let budget = slo::error_budget(&registry, "http_success").unwrap();
    assert_eq!(budget.total, 1000);
    assert_eq!(budget.bad, 0);
    assert!(budget.within);
    assert!((budget.remaining - 1.0).abs() < 1e-9, "预算未消耗");
}

#[test]
fn error_budget_allows_floor_within_budget() {
    let registry = slo::SloRegistry::new();
    for i in 0..1000 {
        slo::check_slo(&registry, "http_success", None, i != 500);
    }
    let budget = slo::error_budget(&registry, "http_success").unwrap();
    assert_eq!(budget.bad, 1);
    assert_eq!(budget.allowed_bad, 1, "99.9% × 1000 = 允许 1 次失败");
    assert!(budget.within, "1/1000 失败仍在预算内");
    assert!((budget.remaining - 0.0).abs() < 1e-9);
}

#[test]
fn error_budget_exhausted_flags_violation() {
    let registry = slo::SloRegistry::new();
    for i in 0..1000 {
        slo::check_slo(&registry, "http_success", None, i >= 3);
    }
    let budget = slo::error_budget(&registry, "http_success").unwrap();
    assert_eq!(budget.bad, 3);
    assert!(!budget.within, "3 次失败超 99.9% 预算 → 不达标");
    let state = registry.get("http_success").unwrap();
    assert_eq!(state.violation_count(), 3);
}

#[test]
fn latency_slo_p95_and_budget() {
    let registry = slo::SloRegistry::new();
    for i in 0..100 {
        let latency = if i < 90 { 6 } else { 60 }; // 90 个 6ms + 10 个 60ms
        slo::check_slo(&registry, "tool_schedule", Some(latency), true);
    }
    let state = registry.get("tool_schedule").unwrap();
    assert_eq!(state.p95_ms(), Some(60), "p95 取第 95 百分位样本（60ms）");
    let budget = state.error_budget();
    assert!(budget.within, "窗口内 10% 越界仍在 p95 预算内（允许 20%）");
    assert_eq!(budget.bad, 10);
    assert_eq!(budget.allowed_bad, 20, "延迟型默认允许 20%（100/5）");
}

#[test]
fn report_json_shape_and_values() {
    let registry = slo::SloRegistry::new();
    slo::check_slo(&registry, "ipc", Some(3), true);
    slo::check_slo(&registry, "ipc", Some(9), true); // 违规
    let report = slo::report(&registry);
    assert_eq!(report["count"], serde_json::json!(5));
    let items = report["slo"].as_array().unwrap();
    let ipc = items
        .iter()
        .find(|item| item["name"] == "ipc")
        .expect("报告应含 ipc");
    assert_eq!(ipc["samples"], serde_json::json!(2));
    assert_eq!(ipc["target_ms"], serde_json::json!(5));
    assert_eq!(ipc["p95_ms"], serde_json::json!(9));
    assert_eq!(ipc["violations"], serde_json::json!(1));
    assert!(ipc["error_budget"]["within"].is_boolean());
    assert!(ipc["achieving"].is_boolean());
    assert!(!ipc["achieving"].as_bool().unwrap(), "违规样本 → 不达标");
}

#[test]
fn report_empty_registry_no_panic() {
    let registry = slo::SloRegistry::new();
    let report = slo::report(&registry);
    assert_eq!(report["count"], serde_json::json!(5));
    assert!(report["slo"].as_array().unwrap().len() == 5);
}

#[test]
fn check_slo_global_records_into_global() {
    slo::reset_global_for_test();
    let within = slo::check_slo_global("panel_wake", Some(100), true);
    assert!(within);
    let within = slo::check_slo_global("panel_wake", Some(200), true);
    assert!(!within, "200ms > 150ms 违规");
    let report = slo::report_global();
    let panel = report["slo"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == "panel_wake")
        .unwrap();
    assert_eq!(panel["samples"], serde_json::json!(2));
    assert_eq!(panel["violations"], serde_json::json!(1));
    assert!(!slo::global_achieving(), "有违规样本 → 全局不达标");
}

#[test]
fn reset_global_for_test_isolates_observations() {
    slo::reset_global_for_test();
    let _ = slo::check_slo_global("ipc", Some(9), true); // 违规
    assert_eq!(slo::global().get("ipc").unwrap().violation_count(), 1);
    slo::reset_global_for_test();
    let state = slo::global().get("ipc").unwrap();
    assert_eq!(state.sample_count(), 0, "重置后样本清零");
    assert_eq!(state.violation_count(), 0);
}

#[test]
fn slo_values_serialize_into_json() {
    let registry = slo::SloRegistry::new();
    let _ = slo::check_slo(&registry, "audit_zero_loss", None, true);
    let json = slo::report(&registry).to_string();
    let parsed: Value = serde_json::from_str(&json).unwrap();
    assert!(parsed["slo"].as_array().unwrap().len() >= 5);
    assert!(parsed["slo"][0]["name"].as_str().is_some());
}
