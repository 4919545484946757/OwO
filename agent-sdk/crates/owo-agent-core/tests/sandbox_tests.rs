//! sandbox.rs 契约测试（X01）：策略越界拒绝、能力显式降级/不可用、
//! 执行器抽象与审计事件（Wave 1 契约）+ 真实探测/门卫/审计扩展（Wave 2）。
//! 自 R6 起 lib.rs 已导出本模块，测试直接使用 lib 导出。

use owo_agent_core::sandbox::*;
use std::path::PathBuf;

fn workspace_dir() -> PathBuf {
    std::env::temp_dir().join(format!("owo-sandbox-ws-{}", uuid::Uuid::new_v4()))
}

fn full_support() -> PlatformSupport {
    PlatformSupport {
        os: "windows".to_string(),
        app_container: true,
        job_object: true,
        low_integrity: true,
        reason: "测试全量能力".to_string(),
    }
}

#[test]
fn default_policy_is_workspace_only_and_validates() {
    let policy = SandboxPolicy::for_workspace("demo", workspace_dir());
    assert_eq!(policy.file_scope, FileScope::WorkspaceOnly);
    assert_eq!(policy.network_policy, NetworkPolicy::None);
    assert_eq!(policy.require_isolation, IsolationLevel::AppContainerJob);
    assert!(!policy.allow_degraded);
    assert!(policy.validate().is_ok());
}

#[test]
fn unrestricted_file_without_flag_rejected() {
    let mut policy = SandboxPolicy::for_workspace("demo", workspace_dir());
    policy.file_scope = FileScope::Unrestricted;
    let err = policy.validate().unwrap_err();
    assert!(matches!(err, SandboxError::PolicyViolation(_)));
}

#[test]
fn unrestricted_network_without_flag_rejected() {
    let mut policy = SandboxPolicy::for_workspace("demo", workspace_dir());
    policy.network_policy = NetworkPolicy::Unrestricted;
    let err = policy.validate().unwrap_err();
    assert!(matches!(err, SandboxError::PolicyViolation(_)));
}

#[test]
fn allowlist_network_requires_hosts() {
    let mut policy = SandboxPolicy::for_workspace("demo", workspace_dir());
    policy.network_policy = NetworkPolicy::AllowList;
    let err = policy.validate().unwrap_err();
    assert!(matches!(err, SandboxError::PolicyViolation(_)));

    policy.allow_hosts = vec!["api.openai.com:443".to_string()];
    assert!(policy.validate().is_ok());
}

#[test]
fn zero_cpu_ms_rejected() {
    let mut policy = SandboxPolicy::for_workspace("demo", workspace_dir());
    policy.cpu_ms = Some(0);
    assert!(policy.validate().is_err());
}

#[test]
fn workspace_only_requires_workspace_path() {
    let policy = SandboxPolicy {
        workspace: None,
        ..SandboxPolicy::default()
    };
    let err = policy.validate().unwrap_err();
    assert!(matches!(err, SandboxError::PolicyViolation(_)));
}

#[test]
fn command_cwd_outside_workspace_rejected() {
    let workspace = workspace_dir();
    let outside = workspace.parent().unwrap().join("outside");
    let policy = SandboxPolicy::for_workspace("demo", workspace.clone());
    let mut command = SandboxCommand::new("app.exe", policy);
    command.cwd = Some(outside);
    assert!(command.validate().is_err());
}

#[test]
fn deny_program_blocked_by_blacklist() {
    let workspace = workspace_dir();
    let policy = SandboxPolicy::for_workspace("demo", workspace);
    let command = SandboxCommand::new("C:\\Windows\\System32\\shutdown.exe", policy);
    let err = command.validate().unwrap_err();
    assert!(matches!(err, SandboxError::PolicyViolation(_)));
}

#[test]
fn deny_hit_is_case_insensitive_substring() {
    let deny = vec!["shutdown".to_string(), "format".to_string()];
    assert_eq!(
        SandboxCommand::deny_hit("C:\\Windows\\System32\\SHUTDOWN.exe", &deny),
        Some("shutdown".to_string())
    );
    assert_eq!(SandboxCommand::deny_hit("npx tsc", &deny), None);
    assert_eq!(
        SandboxCommand::deny_hit("format c:", &deny),
        Some("format".to_string())
    );
}

#[test]
fn probe_is_explicit_and_never_silent() {
    // Wave 2 真实探测：结果必须显式（Full/Degraded 或 Unsupported+原因）。
    let support = probe_platform_support();
    assert!(!support.reason.is_empty());
    let policy = SandboxPolicy::for_workspace("demo", workspace_dir());
    match evaluate_capability(&support, &policy) {
        Ok(CapabilityEvaluation::Full | CapabilityEvaluation::Degraded(_)) => {}
        Err(SandboxError::Unsupported(reason)) => assert!(!reason.is_empty()),
        other => panic!("能力评估必须显式，实际 {:?}", other),
    }
    // 探测事件可审计。
    let manager = SandboxManager::with_probe(Box::new(MockSandboxExecutor::default()));
    assert!(manager
        .audit()
        .contains_kind(SandboxEventKind::CapabilityProbe));
}

#[test]
fn full_support_platform_evaluates_full() {
    let policy = SandboxPolicy::for_workspace("demo", workspace_dir());
    assert_eq!(
        evaluate_capability(&full_support(), &policy).unwrap(),
        CapabilityEvaluation::Full
    );
}

#[test]
fn degraded_isolation_requires_explicit_flag() {
    let policy = SandboxPolicy::for_workspace("demo", workspace_dir());
    let job_only = PlatformSupport {
        os: "windows".to_string(),
        app_container: false,
        job_object: true,
        low_integrity: true,
        reason: "无 AppContainer，仅 Job".to_string(),
    };
    // 默认不允许降级 → 显式 Unsupported。
    assert!(evaluate_capability(&job_only, &policy).is_err());

    // 显式 allow_degraded → 返回 Degraded(JobOnly)。
    let mut degraded_policy = policy.clone();
    degraded_policy.allow_degraded = true;
    assert_eq!(
        evaluate_capability(&job_only, &degraded_policy).unwrap(),
        CapabilityEvaluation::Degraded(IsolationLevel::JobOnly)
    );
}

#[test]
fn manager_spawn_ok_path_with_mock_executor() {
    let executor = MockSandboxExecutor::default();
    let mut manager = SandboxManager::new(Box::new(executor), full_support());
    let command = SandboxCommand::new(
        "app.exe",
        SandboxPolicy::for_workspace("demo", workspace_dir()),
    );
    let process = manager.spawn(&command).unwrap();
    assert_eq!(process.handle.id, "mock-demo");
    assert_eq!(process.status, SandboxProcessStatus::Running);
    assert!(manager.audit().is_empty());
}

#[test]
fn manager_rejects_policy_violation_and_audits() {
    let executor = MockSandboxExecutor::default();
    let mut manager = SandboxManager::new(Box::new(executor), full_support());
    let mut policy = SandboxPolicy::for_workspace("demo", workspace_dir());
    policy.file_scope = FileScope::Unrestricted;
    let command = SandboxCommand::new("app.exe", policy);
    assert!(manager.spawn(&command).is_err());
    assert!(manager
        .audit()
        .contains_kind(SandboxEventKind::SpawnRejected));
}

#[test]
fn manager_reports_unsupported_isolation_and_audits() {
    let executor = MockSandboxExecutor::default();
    let support = PlatformSupport {
        os: "linux".to_string(),
        app_container: false,
        job_object: false,
        low_integrity: false,
        reason: "平台不支持 OS 级沙箱".to_string(),
    };
    let mut manager = SandboxManager::new(Box::new(executor), support);
    let command = SandboxCommand::new("app", SandboxPolicy::for_workspace("demo", workspace_dir()));
    assert!(manager.spawn(&command).is_err());
    assert!(manager
        .audit()
        .contains_kind(SandboxEventKind::UnsupportedIsolation));
}

#[test]
fn manager_records_degradation_audit_event() {
    let executor = MockSandboxExecutor::with_isolation(IsolationLevel::JobOnly);
    let support = PlatformSupport {
        os: "windows".to_string(),
        app_container: false,
        job_object: true,
        low_integrity: true,
        reason: "无 AppContainer，仅 Job".to_string(),
    };
    let mut manager = SandboxManager::new(Box::new(executor), support);
    let mut policy = SandboxPolicy::for_workspace("demo", workspace_dir());
    policy.allow_degraded = true;
    policy.require_isolation = IsolationLevel::AppContainerJob;
    let command = SandboxCommand::new("app.exe", policy);
    assert!(manager.spawn(&command).is_ok());
    assert!(manager
        .audit()
        .contains_kind(SandboxEventKind::DegradedIsolation));
}

#[test]
fn manager_kill_and_unhealthy_audit() {
    let executor = MockSandboxExecutor::default();
    let mut manager = SandboxManager::new(Box::new(executor), full_support());
    let handle = SandboxHandle {
        id: "mock-p1".to_string(),
        spawned_at: "t".to_string(),
    };
    manager.kill(&handle).unwrap();
    assert!(manager.audit().contains_kind(SandboxEventKind::Killed));

    let health = manager.check_healthy();
    assert!(health.healthy);
    assert!(!manager.audit().contains_kind(SandboxEventKind::Unhealthy));

    // 执行器不健康 → 审计 Unhealthy。
    let executor = MockSandboxExecutor::default();
    executor
        .healthy
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let mut manager = SandboxManager::new(Box::new(executor), full_support());
    assert!(!manager.check_healthy().healthy);
    assert!(manager.audit().contains_kind(SandboxEventKind::Unhealthy));
}

#[test]
fn manager_executor_spawn_failure_audited() {
    let executor = MockSandboxExecutor::default();
    executor
        .spawn_should_fail
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let mut manager = SandboxManager::new(Box::new(executor), full_support());
    let command = SandboxCommand::new(
        "app.exe",
        SandboxPolicy::for_workspace("demo", workspace_dir()),
    );
    assert!(manager.spawn(&command).is_err());
    assert!(manager
        .audit()
        .contains_kind(SandboxEventKind::SpawnRejected));
}

#[test]
fn manager_with_probe_records_capability_probe_audit() {
    let executor = MockSandboxExecutor::default();
    let manager = SandboxManager::with_probe(Box::new(executor));
    assert!(manager
        .audit()
        .contains_kind(SandboxEventKind::CapabilityProbe));
    // 探测结果必须携带原因（保守降级，不静默）。
    assert!(!manager.platform().reason.is_empty());
}

#[test]
fn inside_workspace_helper() {
    let workspace = workspace_dir();
    assert!(inside_workspace(&workspace, &workspace.join("a/b.txt")));
    assert!(!inside_workspace(
        &workspace,
        &workspace.parent().unwrap().join("secret.txt")
    ));
}

#[test]
fn guard_blocks_policy_violation_without_spawn() {
    let executor = MockSandboxExecutor::default();
    let mut manager = SandboxManager::new(Box::new(executor), full_support());
    let mut policy = SandboxPolicy::for_workspace("demo", workspace_dir());
    policy.file_scope = FileScope::Unrestricted;
    let command = SandboxCommand::new("app.exe", policy);
    assert!(manager.guard(&command).is_err());
    assert!(manager
        .audit()
        .contains_kind(SandboxEventKind::SpawnRejected));
}

#[test]
fn guard_allows_full_and_records_degradation() {
    let executor = MockSandboxExecutor::default();
    let support = PlatformSupport {
        os: "windows".to_string(),
        app_container: false,
        job_object: true,
        low_integrity: true,
        reason: "无 AppContainer，仅 Job".to_string(),
    };
    let mut manager = SandboxManager::new(Box::new(executor), support);

    // 策略要求 AppContainer 但平台仅 Job；未允许降级 → Blocked。
    let strict_policy = SandboxPolicy::for_workspace("demo", workspace_dir());
    let command = SandboxCommand::new("app.exe", strict_policy);
    assert!(manager.guard(&command).is_err());

    // 允许降级 → Allowed + Degraded 审计。
    let mut policy = SandboxPolicy::for_workspace("demo", workspace_dir());
    policy.allow_degraded = true;
    let command = SandboxCommand::new("app.exe", policy);
    match manager.guard(&command).unwrap() {
        ExecGuard::Allowed {
            degraded: Some(IsolationLevel::JobOnly),
        } => {}
        other => panic!("期望 Degraded(JobOnly)，实际 {:?}", other),
    }
    assert!(manager
        .audit()
        .contains_kind(SandboxEventKind::DegradedIsolation));
}

#[test]
fn take_audit_events_drains_log() {
    let executor = MockSandboxExecutor::default();
    let mut manager = SandboxManager::new(Box::new(executor), full_support());
    let mut policy = SandboxPolicy::for_workspace("demo", workspace_dir());
    policy.file_scope = FileScope::Unrestricted;
    let command = SandboxCommand::new("app.exe", policy);
    let _ = manager.spawn(&command);
    let events = manager.take_audit_events();
    assert!(!events.is_empty());
    assert!(manager.audit().is_empty());
}

#[test]
fn unavailable_executor_is_explicit_not_silent() {
    let executor = UnavailableExecutor {
        reason: "测试环境无沙箱".to_string(),
    };
    assert_eq!(executor.capability(), IsolationLevel::None);
    let command = SandboxCommand::new(
        "app.exe",
        SandboxPolicy::for_workspace("demo", workspace_dir()),
    );
    let err = executor.spawn(&command).unwrap_err();
    assert!(matches!(err, SandboxError::Unsupported(_)));
    assert!(!executor.check_healthy().healthy);
}

#[test]
fn sandbox_event_kind_labels_for_audit_chain() {
    assert_eq!(
        SandboxEventKind::CapabilityProbe.label(),
        "capability_probe"
    );
    assert_eq!(SandboxEventKind::SpawnRejected.label(), "spawn_rejected");
    assert_eq!(
        SandboxEventKind::UnsupportedIsolation.label(),
        "unsupported_isolation"
    );
    assert_eq!(
        SandboxEventKind::DegradedIsolation.label(),
        "degraded_isolation"
    );
    assert_eq!(SandboxEventKind::Killed.label(), "killed");
    assert_eq!(SandboxEventKind::Unhealthy.label(), "unhealthy");
    assert_eq!(SandboxEventKind::Attached.label(), "attached");
}

#[test]
fn mock_wait_output_returns_cached_status() {
    let executor = MockSandboxExecutor::default();
    let mut manager = SandboxManager::new(Box::new(executor), full_support());
    let command = SandboxCommand::new(
        "app.exe",
        SandboxPolicy::for_workspace("demo", workspace_dir()),
    );
    let mut process = manager.spawn(&command).unwrap();
    process.stdout = b"hello".to_vec();
    process.status = SandboxProcessStatus::Exited(0);
    let info = process.wait_output().unwrap();
    assert_eq!(info.exit_code, 0);
    assert_eq!(info.stdout, b"hello");
}
