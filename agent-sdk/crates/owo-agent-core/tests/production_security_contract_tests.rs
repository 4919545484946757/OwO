//! 生产级安全边界契约测试（存储 / 审计 / 凭据托管 / 沙箱最小安全语义）。
//!
//! 覆盖目标（综合技术文档 §4.8 / §6.1 P0 清单）：
//! - settings 与 ProviderConfig 序列化绝不出现明文凭据；
//! - 审计链任意篡改（改字段 / 删记录 / 重排 / 篡改锚点）可被离线校验检出；
//! - 凭据库不可用时显式失败，绝不静默回退写入 settings / 未托管密钥；
//! - 沙箱不支持或降级时必须显式报告；不存在“挂接失败但子进程继续运行”的路径；
//! - Windows Job Object 实证走环境门控，非 Windows / 能力缺失时显式跳过，避免 CI 偶发失败。
//!
//! 全部使用临时数据目录与随机生成的密钥 / token，不触碰真实模型或用户凭据。

use owo_agent_core::audit_chain::*;
use owo_agent_core::credentials::*;
use owo_agent_core::sandbox::*;

/// 随机密钥（2×uuid 字节，32 字节），保证每次运行独立、不依赖固定测试向量。
fn random_key() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes
}

/// 随机临时目录（core 无 tempfile 依赖，沿用 std + uuid）。
fn temp_dir(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("owo-sec-{label}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------- settings / ProviderConfig 零明文凭据 ----------

/// Provider 配置序列化永不内联明文（settings.json 契约）；内联仅在测试路径存在。
#[test]
fn provider_config_never_serializes_plaintext_secrets() {
    let inline_secret = format!("sk-inline-secret-{}", uuid::Uuid::new_v4());
    let config = ProviderConfig {
        name: "openai".to_string(),
        api_key_ref: Some(ApiKeyRef {
            store_key: Some("openai-cred".to_string()),
            env_var: Some("OPENAI_API_KEY".to_string()),
            inline: Some(inline_secret.clone()),
        }),
        base_url: Some("https://api.openai.com/v1".to_string()),
        model: Some("gpt-5".to_string()),
    };
    let json = config.serialized_without_plaintext().unwrap();
    assert!(!json.contains(&inline_secret), "序列化内联明文：{json}");
    assert!(
        scan_json_for_secrets(&json, &[&inline_secret]).is_empty(),
        "明文扫描必须零命中"
    );
    // 引用与其它非敏感字段保留（可审计、可重建引用）。
    assert!(json.contains("openai-cred"));
    assert!(json.contains("OPENAI_API_KEY"));
    assert!(json.contains("gpt-5"));

    // 反序列化后 inline 为空（引用型恢复），无明文回填。
    let loaded: ProviderConfig = serde_json::from_str(&json).unwrap();
    assert!(loaded.api_key_ref.as_ref().unwrap().inline.is_none());
}

/// 明文扫描门禁：真泄漏必须被检出（正向 + 负向）。
#[test]
fn settings_scan_detects_plaintext_leaks() {
    let secret = "sk-live-secret-abcdef";
    let dirty = format!(r#"{{"api_key":"{secret}"}}"#);
    assert_eq!(
        scan_json_for_secrets(&dirty, &[secret]),
        vec![secret.to_string()],
        "明文凭据必须被扫描检出"
    );
    let clean = r#"{"api_key_ref":{"store_key":"openai-cred"}}"#;
    assert!(scan_json_for_secrets(clean, &[secret]).is_empty());
}

// ---------- 审计链防篡改 ----------

fn sample_record(actor: &str, event: &str, detail: &str) -> AuditRecord {
    AuditRecord::new(actor, event, detail)
}

/// 任意篡改（改字段 / 删记录 / 重排 / 篡改锚点）都可被离线校验检出。
#[test]
fn audit_chain_detects_any_tampering() {
    let key = random_key();

    // 基准：完好链校验通过。
    let mut chain = AuditChain::new(&key, 2);
    for i in 0..4 {
        chain.append(sample_record("agent", "tool_call", &format!("读取 {i}")));
    }
    assert!(chain.verify().is_ok());
    assert!(verify_export(&chain.export(), &key).is_ok());

    // 改字段。
    let mut export = chain.export();
    export.records[1].record.detail = "篡改 detail".to_string();
    assert!(verify_export(&export, &key).is_err());

    // 改 actor。
    let mut export = chain.export();
    export.records[0].record.actor = "attacker".to_string();
    assert!(verify_export(&export, &key).is_err());

    // 删记录。
    let mut export = chain.export();
    export.records.remove(1);
    assert!(verify_export(&export, &key).is_err());

    // 重排。
    let mut export = chain.export();
    export.records.swap(0, 1);
    assert!(verify_export(&export, &key).is_err());

    // 篡改锚点（把分段链哈希换成伪造值）。
    let mut export = chain.export();
    export.anchors[0].hash = "ff".repeat(32);
    assert!(verify_export(&export, &key).is_err());

    // 错误密钥永远无法校验通过。
    let wrong_key = random_key();
    assert!(verify_export(&chain.export(), &wrong_key).is_err());
}

/// 审计导出与原链：绝不泄漏托管密钥或其摘要（密钥与导出文件分离）。
#[test]
fn audit_export_never_leaks_managed_key_or_secret() {
    let store = MemoryCredentialStore::new();
    let key_name = format!("audit-key-{}", uuid::Uuid::new_v4());
    let mut chain = AuditChain::from_managed_key(&store, &key_name, 2).unwrap();
    chain.append(sample_record("sec", "tool_call", "读取 main.rs"));
    chain.append(sample_record("sec", "approve", "批准写入").with_tool("write_file"));

    let key_hex = store.get(&key_name).unwrap();
    let digest = chain.key_digest();
    let export = chain.export();
    let json = serde_json::to_string(&export).unwrap();
    assert!(!json.contains(&key_hex), "审计导出泄露了托管密钥");
    assert!(!json.contains(&digest), "审计导出泄露了密钥摘要");

    // 导出可离线校验；密钥入库（hex，64 字符）。
    assert_eq!(key_hex.len(), 64);
    let key = decode_hex(&key_hex);
    assert!(verify_export(&export, &key).is_ok());
    assert!(chain.verify().is_ok());
}

/// 托管密钥复用：同一凭据库名两次构造得到同一链密钥（重启语义），校验一致。
#[test]
fn audit_managed_key_reuses_and_verifies_across_restart() {
    let store = MemoryCredentialStore::new();
    let key_name = format!("audit-key-{}", uuid::Uuid::new_v4());
    let mut first = AuditChain::from_managed_key(&store, &key_name, 10).unwrap();
    first.append(sample_record("agent", "a", "1"));
    let second = AuditChain::from_managed_key(&store, &key_name, 10).unwrap();
    assert_eq!(first.key_digest(), second.key_digest());
    let key_hex = store.get(&key_name).unwrap();
    assert!(verify_export(&first.export(), &decode_hex(&key_hex)).is_ok());
}

// ---------- 凭据库不可用 → 明确失败，绝不回退 ----------

/// 凭据库不可用：审计链密钥 / DEK 托管 / 解析全部显式失败，绝不静默回退。
#[test]
fn credential_store_unavailable_fails_explicitly_no_settings_fallback() {
    let unavailable = UnavailableStore {
        reason: "凭据库未接入",
    };

    // 审计链密钥托管：不可用 → 显式错误（拒绝无托管密钥的静默回退）。
    let chain_err = AuditChain::from_managed_key(&unavailable, "audit-key", 10).unwrap_err();
    assert!(matches!(chain_err, AuditChainError::Invalid(_)));
    // 强制轮换托管密钥同样拒绝。
    let rotate_err =
        AuditChain::force_rotate_managed_key(&unavailable, "audit-key", 10).unwrap_err();
    assert!(matches!(rotate_err, AuditChainError::Invalid(_)));

    // DEK 托管：不可用 → StoreUnavailable。
    let dek_err = managed_dek(&unavailable, "dek").unwrap_err();
    assert!(matches!(dek_err, CredentialError::StoreUnavailable(_)));

    // 解析（无凭据库 + 无环境变量 + 无内联）→ 显式 Missing，不产出可落盘明文。
    let resolver = CredentialResolver::no_store();
    let missing_env = format!("OWO_TEST_UNSET_{}", uuid::Uuid::new_v4().simple());
    std::env::remove_var(&missing_env);
    let resolve_err = resolver
        .resolve(&ApiKeyRef::from_env(&missing_env))
        .unwrap_err();
    assert!(matches!(resolve_err, CredentialError::Missing(_)));
}

/// 凭据库不可用时，ProviderConfig 只保留引用，绝不把明文写回可落盘 JSON。
#[test]
fn unavailable_store_never_writes_plaintext_back_to_settings() {
    let store = MemoryCredentialStore::with_available(false);
    let resolver = CredentialResolver::new(Box::new(store));
    let config = ProviderConfig::with_store_key("openai", "openai-cred");

    // 序列化只含引用，绝无明文。
    let json = config.serialized_without_plaintext().unwrap();
    assert!(json.contains("openai-cred"));
    assert!(!json.contains("sk-"), "凭据 JSON 出现疑似明文：{json}");

    // 凭据库不可用 → 解析显式失败（不静默产出可写回 settings 的明文）。
    let err = resolver
        .resolve(config.api_key_ref.as_ref().unwrap())
        .unwrap_err();
    assert!(matches!(err, CredentialError::Missing(_)));
}

// ---------- 沙箱最小安全语义 ----------

/// 平台探测与能力评估绝不静默：结果必须显式携带原因。
#[test]
fn sandbox_platform_probe_is_explicit_never_silent() {
    let support = probe_platform_support();
    assert!(
        !support.reason.is_empty(),
        "平台探测必须携带 reason（禁用静默假装安全）"
    );
    let policy = SandboxPolicy::for_workspace("demo", temp_dir("probe"));
    match evaluate_capability(&support, &policy) {
        Ok(CapabilityEvaluation::Full | CapabilityEvaluation::Degraded(_)) => {}
        Err(SandboxError::Unsupported(reason)) => assert!(!reason.is_empty()),
        other => panic!("能力评估必须显式，实际 {other:?}"),
    }
}

/// 不支持时显式报错 + 审计事件，绝不返回“可执行”的假结论。
#[test]
fn unavailable_sandbox_reports_unsupported_explicitly() {
    let executor = UnavailableExecutor {
        reason: "测试环境无 OS 沙箱".to_string(),
    };
    let support = PlatformSupport {
        os: "linux".to_string(),
        app_container: false,
        job_object: false,
        low_integrity: false,
        reason: "平台不支持 OS 沙箱".to_string(),
    };
    let mut manager = SandboxManager::new(Box::new(executor), support);
    let command = SandboxCommand::new("app", SandboxPolicy::for_workspace("demo", temp_dir("u")));
    let err = manager.spawn(&command).unwrap_err();
    assert!(matches!(err, SandboxError::Unsupported(_)));
    assert!(manager
        .audit()
        .contains_kind(SandboxEventKind::UnsupportedIsolation));
}

/// 不存在“挂接失败但子进程继续运行”的路径：attach 失败必须显式报错且不发放守卫。
#[test]
fn attach_failure_yields_no_guard_explicitly() {
    // Mock 执行器默认 attach 返回 Unsupported（不假装挂接成功）。
    let executor = MockSandboxExecutor::default();
    let mut manager = SandboxManager::new(Box::new(executor), full_support());
    let policy = SandboxPolicy::for_workspace("attach", temp_dir("attach"));
    let err = manager.attach_pid(&policy, 1234).unwrap_err();
    assert!(
        matches!(err, SandboxError::Unsupported(_)),
        "挂接失败必须显式 Unsupported，实际 {err:?}"
    );
    // 失败路径不产生任何 JobGuard（调用方只能拿到 Err，无法继续运行子进程）。
}

/// 网络白名单 / 无限制策略无法被 Job 挂接路径强制 → 挂接前显式拒绝（不静默放开网络）。
#[test]
fn attach_with_network_policy_is_rejected_before_attach() {
    let executor = MockSandboxExecutor::default();
    let mut manager = SandboxManager::new(Box::new(executor), full_support());
    let mut policy = SandboxPolicy::for_workspace("net", temp_dir("net"));
    policy.network_policy = NetworkPolicy::AllowList;
    policy.allow_hosts = vec!["api.example.com".to_string()];
    assert!(network_requires_app_container(&policy));
    let err = manager.attach_pid(&policy, 1234).unwrap_err();
    assert!(matches!(err, SandboxError::Unsupported(_)));
    // 拒绝发生在能力评估/执行器之前：审计不应把该进程标记为“已挂接”。
    assert!(!manager.audit().contains_kind(SandboxEventKind::Attached));
}

// ---------- Windows Job Object 实证（环境门控，避免 CI 偶发失败） ----------

/// 环境门控：非 Windows 或 Job Object 不可用时显式跳过（打印原因）。
/// `OWO_FORCE_OS_TESTS=1` 时把跳过升级为失败（严格模式，用于真实 Windows 主机验证）。
fn job_object_available() -> bool {
    if !cfg!(target_os = "windows") {
        eprintln!("SKIP: 非 Windows 平台，Job Object 实证显式跳过");
        return false;
    }
    let support = probe_platform_support();
    if !support.job_object {
        eprintln!(
            "SKIP: Job Object 不可用（{}），实证显式跳过",
            support.reason
        );
        if std::env::var("OWO_FORCE_OS_TESTS").as_deref() == Ok("1") {
            panic!(
                "OWO_FORCE_OS_TESTS=1 且 Job Object 不可用：{}",
                support.reason
            );
        }
        return false;
    }
    true
}

/// Windows 下经真实 Job Object spawn 一个进程并采集输出（能力缺失时整体跳过）。
#[test]
fn windows_job_object_spawn_is_env_gated() {
    if !job_object_available() {
        return;
    }
    let manager = default_manager();
    let policy = SandboxPolicy {
        file_scope: FileScope::WorkspacePlusReadonlySystem,
        network_policy: NetworkPolicy::Loopback,
        require_isolation: IsolationLevel::JobOnly,
        allow_degraded: true,
        ..SandboxPolicy::default()
    };
    let command = SandboxCommand::new("cmd", policy)
        .with_args(vec!["/C".to_string(), "echo job-object-ok".to_string()]);
    let mut process = {
        let mut manager = manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        manager
            .spawn(&command)
            .expect("Job Object 沙箱 spawn 应成功")
    };
    let info = process.wait_output().expect("等待进程应成功");
    assert_eq!(info.exit_code, 0);
    let stdout = String::from_utf8_lossy(&info.stdout);
    assert!(stdout.contains("job-object-ok"), "stdout：{stdout:?}");
}

// ---------- 工具 ----------

fn full_support() -> PlatformSupport {
    PlatformSupport {
        os: "windows".to_string(),
        app_container: true,
        job_object: true,
        low_integrity: true,
        reason: "测试全量能力".to_string(),
    }
}

fn decode_hex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16).unwrap() as u8;
            let low = (pair[1] as char).to_digit(16).unwrap() as u8;
            (high << 4) | low
        })
        .collect()
}
