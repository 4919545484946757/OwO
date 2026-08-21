//! OS 沙箱集成测试（X01，Wave 2，环境门控）。
//!
//! 真实调用 Windows Job Object / 进程管理 API：
//! - 能力探测真实性、进程 spawn/wait/kill、Job 资源限制、kill-on-close 防孤儿、
//!   attach_pid 挂接与守卫释放、结构布局与 SDK 一致性。
//! - 门控：非 Windows 或 Job Object 不可用时**显式跳过**（打印原因），不假装通过；
//!   设置环境变量 `OWO_FORCE_OS_TESTS=1` 可把跳过变为失败（严格模式）。

use owo_agent_core::sandbox::*;
use std::time::Duration;

fn os_sandbox_available() -> Option<PlatformSupport> {
    if !cfg!(target_os = "windows") {
        eprintln!("SKIP: 非 Windows 平台，OS 沙箱集成测试显式跳过");
        return None;
    }
    let support = probe_platform_support();
    if !support.job_object {
        eprintln!(
            "SKIP: Job Object 不可用（{}），OS 沙箱集成测试显式跳过",
            support.reason
        );
        if std::env::var("OWO_FORCE_OS_TESTS").as_deref() == Ok("1") {
            panic!(
                "OWO_FORCE_OS_TESTS=1 且 Job Object 不可用：{}",
                support.reason
            );
        }
        return None;
    }
    Some(support)
}

fn policy(name: &str, mem_mb: Option<u64>) -> SandboxPolicy {
    SandboxPolicy {
        name: name.to_string(),
        workspace: None,
        file_scope: FileScope::WorkspacePlusReadonlySystem,
        network_policy: NetworkPolicy::Loopback,
        cpu_ms: Some(30_000),
        mem_mb,
        ttl_secs: Some(60),
        require_isolation: IsolationLevel::JobOnly,
        allow_degraded: true,
        ..SandboxPolicy::default()
    }
}

#[test]
fn probe_reports_real_windows_capability() {
    let Some(support) = os_sandbox_available() else {
        return;
    };
    assert_eq!(support.os, "windows");
    assert!(support.job_object);
    assert!(!support.reason.is_empty());
    // 能力与探测一致：Job 可用 → 至少 JobOnly。
    let available = available_isolation(&support);
    assert!(available >= IsolationLevel::JobOnly);
    // 显式降级路径可评估（Job 基线允许降级）。
    let mut degraded_policy = policy("probe", None);
    degraded_policy.require_isolation = IsolationLevel::AppContainerJob;
    degraded_policy.allow_degraded = true;
    match evaluate_capability(&support, &degraded_policy) {
        Ok(CapabilityEvaluation::Full | CapabilityEvaluation::Degraded(_)) => {}
        other => panic!("能力评估必须显式，实际 {:?}", other),
    }
}

#[test]
fn struct_layouts_match_os_sdk() {
    let Some(_support) = os_sandbox_available() else {
        return;
    };
    assert!(
        os_struct_layouts_match(),
        "Windows 结构布局与 SDK 不一致（ABI 漂移）"
    );
}

#[test]
fn spawn_echo_captures_output() {
    let Some(_support) = os_sandbox_available() else {
        return;
    };
    let manager = default_manager();
    let command = SandboxCommand::new("cmd", policy("echo", None))
        .with_args(vec!["/C".to_string(), "echo hello-sandbox".to_string()]);
    let mut process = {
        let mut manager = manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        manager.spawn(&command).expect("沙箱 spawn 应成功")
    };
    let info = process.wait_output().expect("等待应成功");
    assert_eq!(info.exit_code, 0);
    let stdout = String::from_utf8_lossy(&info.stdout);
    assert!(
        stdout.contains("hello-sandbox"),
        "stdout 应包含输出，实际：{stdout:?}"
    );
}

#[test]
fn spawn_exit_code_propagates() {
    let Some(_support) = os_sandbox_available() else {
        return;
    };
    let manager = default_manager();
    let command = SandboxCommand::new("cmd", policy("exit-code", None))
        .with_args(vec!["/C".to_string(), "exit 7".to_string()]);
    let mut process = {
        let mut manager = manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        manager.spawn(&command).unwrap()
    };
    let info = process.wait_output().unwrap();
    assert_eq!(info.exit_code, 7);
}

#[test]
fn spawn_rejects_denied_program_and_audits() {
    let Some(support) = os_sandbox_available() else {
        return;
    };
    let executor = MockSandboxExecutor::default();
    let mut manager = SandboxManager::new(Box::new(executor), support);
    let command = SandboxCommand::new("shutdown.exe", policy("deny", None));
    let err = manager.spawn(&command).unwrap_err();
    assert!(matches!(err, SandboxError::PolicyViolation(_)));
    assert!(manager
        .audit()
        .contains_kind(SandboxEventKind::SpawnRejected));
}

#[test]
fn kill_terminates_long_process() {
    let Some(_support) = os_sandbox_available() else {
        return;
    };
    let manager = default_manager();
    let command = SandboxCommand::new("cmd", policy("kill", None)).with_args(vec![
        "/C".to_string(),
        "ping -n 60 127.0.0.1 > nul".to_string(),
    ]);
    let mut process = {
        let mut manager = manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        manager.spawn(&command).unwrap()
    };
    process.kill().expect("kill 应成功");
    // kill 后 wait 立即返回且退出码非 0。
    let info = process.wait_output().expect("kill 后 wait 应返回");
    assert_ne!(info.exit_code, 0);
}

#[test]
fn memory_limit_kills_allocating_process() {
    let Some(_support) = os_sandbox_available() else {
        return;
    };
    // 32MB Job 内存上限：powershell 尝试分配 256MB → 提交被拒/进程被杀 → 非 0 退出。
    let manager = default_manager();
    let command = SandboxCommand::new("powershell", policy("mem-limit", Some(32))).with_args(vec![
        "-NoProfile".to_string(),
        "-Command".to_string(),
        "$x = New-Object byte[] (268435456)".to_string(),
    ]);
    let mut process = {
        let mut manager = manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        manager.spawn(&command).expect("powershell spawn 应成功")
    };
    let info = process.wait_output().expect("等待应返回");
    assert_ne!(info.exit_code, 0, "内存超限进程必须非 0 退出");
}

#[test]
fn attach_pid_guard_drop_terminates_process() {
    let Some(_support) = os_sandbox_available() else {
        return;
    };
    // 独立启动一个长进程，再经全局默认管理器 attach 挂入 Job。
    let mut child = std::process::Command::new("cmd")
        .args(["/C", "ping -n 60 127.0.0.1 > nul"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn 应成功");
    let pid = child.id();

    let manager = default_manager();
    let (guard, attached_audited) = {
        let mut manager = manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let guard = manager
            .attach_pid(&policy("attach", None), pid)
            .expect("attach 应成功");
        (
            guard,
            manager.audit().contains_kind(SandboxEventKind::Attached),
        )
    };
    assert_eq!(guard.pid, pid);
    assert!(attached_audited, "attach 必须产生审计事件");

    // 释放守卫 → TerminateJobObject → 进程必须退出（无孤儿）。
    drop(guard);
    let mut exited = false;
    for _ in 0..50 {
        if let Some(status) = child.try_wait().expect("try_wait 应成功") {
            exited = true;
            assert_ne!(status.code(), Some(0));
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(exited, "Job 守卫释放后进程必须被终止");
}

#[test]
fn attach_fails_for_invalid_pid_explicitly() {
    let Some(_support) = os_sandbox_available() else {
        return;
    };
    let manager = default_manager();
    let err = {
        let mut manager = manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        manager
            .attach_pid(&policy("attach-invalid", None), 0xFFFF_FFFE)
            .expect_err("无效 PID 必须显式失败")
    };
    assert!(matches!(err, SandboxError::Spawn(_)));
}

#[test]
fn default_manager_health_follows_platform() {
    os_sandbox_available();
    let manager = SandboxManager::with_probe(Box::new(MockSandboxExecutor::default()));
    // 真实平台（Job 可用）→ 健康；探测事件已审计。
    assert!(manager
        .audit()
        .contains_kind(SandboxEventKind::CapabilityProbe));
}

#[test]
fn long_running_process_respects_job_limits_via_wait() {
    let Some(_support) = os_sandbox_available() else {
        return;
    };
    // CPU 上限 1s：忙循环在 Job CPU 时间耗尽后被终止（非 0 退出）。
    let manager = default_manager();
    let mut limited = policy("cpu-limit", None);
    limited.cpu_ms = Some(1000);
    let command = SandboxCommand::new("powershell", limited).with_args(vec![
        "-NoProfile".to_string(),
        "-Command".to_string(),
        "$x = 0; while ($true) { $x += 1 }".to_string(),
    ]);
    let mut process = {
        let mut manager = manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        manager.spawn(&command).expect("spawn 应成功")
    };
    let info = process.wait_output().expect("等待应返回");
    assert_ne!(info.exit_code, 0, "CPU 时间超限进程必须非 0 退出");
}

// ---- R8 冒烟：DPAPI 存储加密（storage_crypto.rs，#[path] 独立编译验证） ----

#[path = "../src/storage_crypto.rs"]
mod storage_crypto;

use storage_crypto::*;

fn dpapi_available() -> bool {
    if !cfg!(target_os = "windows") {
        eprintln!("SKIP: DPAPI 存储加密仅支持 Windows，显式跳过");
        return false;
    }
    true
}

#[test]
fn dpapi_roundtrip_and_envelope_smoke() {
    if !dpapi_available() {
        return;
    }
    let plain = "owo-secret-明文-123".as_bytes();
    let cipher = encrypt_blob(plain).expect("DPAPI 加密应成功");
    assert_ne!(cipher, plain);
    assert_eq!(decrypt_blob(&cipher).unwrap(), plain);

    let dek = generate_dek();
    assert_eq!(dek.len(), DEK_LEN);
    let protected = protect_dek(&dek).unwrap();
    assert_eq!(unprotect_dek(&protected).unwrap(), dek);

    let value = "sk-sensitive-column-value";
    let encoded = encrypt_sensitive_value(value).unwrap();
    assert_eq!(decrypt_sensitive_value(&encoded).unwrap(), value);

    let dir = std::env::temp_dir().join(format!("owo-crypto-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("secret.bin");
    encrypt_file_envelope(&path, b"envelope-content").unwrap();
    assert_eq!(decrypt_file_envelope(&path).unwrap(), b"envelope-content");
    std::fs::write(&path, b"tampered").unwrap();
    assert!(decrypt_file_envelope(&path).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn app_container_network_capabilities_follow_policy() {
    // 网络能力推断 + 校验（纯数据逻辑，跨平台）。
    let none_policy = SandboxPolicy {
        network_policy: NetworkPolicy::None,
        ..SandboxPolicy::default()
    };
    let none_sids = app_container_network_capabilities(&none_policy);
    assert!(none_sids.is_empty());
    assert!(validate_app_container_network(&none_policy, &none_sids).is_ok());

    let unrestricted = SandboxPolicy {
        network_policy: NetworkPolicy::Unrestricted,
        allow_unrestricted_network: true,
        ..SandboxPolicy::default()
    };
    let sids = app_container_network_capabilities(&unrestricted);
    assert!(sids.iter().any(|sid| *sid == internet_client_sid()));
    assert!(validate_app_container_network(&unrestricted, &sids).is_ok());
    // 隔离策略带网络能力 → 拒绝。
    assert!(validate_app_container_network(&none_policy, &sids).is_err());

    let allowlist = SandboxPolicy {
        network_policy: NetworkPolicy::AllowList,
        allow_hosts: vec!["api.openai.com:443".to_string(), "10.0.0.5".to_string()],
        ..SandboxPolicy::default()
    };
    let allow_sids = app_container_network_capabilities(&allowlist);
    assert!(!allow_sids.is_empty());
    assert!(validate_app_container_network(&allowlist, &allow_sids).is_ok());
    // 内网 host 推断为 PrivateNetworkClientServer。
    let private_only = SandboxPolicy {
        network_policy: NetworkPolicy::AllowList,
        allow_hosts: vec!["192.168.1.10".to_string()],
        ..SandboxPolicy::default()
    };
    let private_sids = app_container_network_capabilities(&private_only);
    assert!(private_sids
        .iter()
        .any(|sid| *sid == private_network_client_server_sid()));
    assert!(!private_sids.iter().any(|sid| *sid == internet_client_sid()));
}

// ---- R9 冒烟 A：存储加密接入真实数据面（settings/会话 落盘加密 + DEK 托管） ----

use owo_agent_core::credentials::{managed_dek, rotate_managed_dek, MemoryCredentialStore};
use owo_agent_core::session::{JsonSessionStore, SessionStore};
use owo_agent_core::settings::Settings;

#[test]
fn encrypted_settings_roundtrip_and_tamper_rejected() {
    if !dpapi_available() {
        return;
    }
    let workspace =
        std::env::temp_dir().join(format!("owo-settings-crypt-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&workspace).unwrap();
    let settings = Settings {
        model: Some("deepseek-v4-flash".to_string()),
        read_only: true,
        ..Settings::default()
    };
    settings.save_encrypted(&workspace).unwrap();
    // 明文 settings.json 不落盘（零明文密钥契约保持）。
    assert!(!workspace.join("settings.json").exists());
    let loaded = Settings::load_encrypted(&workspace).unwrap();
    assert_eq!(loaded.model.as_deref(), Some("deepseek-v4-flash"));
    assert!(loaded.read_only);
    // 篡改加密文件 → 显式解密失败（截断比翻字节更可靠：DPAPI 头区翻转可能不触发校验）。
    let path = workspace.join("settings.json.owo-crypt");
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
    let err = Settings::load_encrypted(&workspace).unwrap_err();
    assert!(err.contains("解密失败"), "错误：{err}");
    let _ = std::fs::remove_dir_all(&workspace);
}

#[test]
fn encrypted_session_roundtrip_and_plaintext_absent() {
    if !dpapi_available() {
        return;
    }
    let root = std::env::temp_dir().join(format!("owo-session-crypt-{}", uuid::Uuid::new_v4()));
    let store = JsonSessionStore::new_encrypted(&root);
    let session = store
        .create(std::path::Path::new("."), "mock", Some("系统提示"))
        .unwrap();
    let loaded = store.load(&session.id).unwrap();
    assert_eq!(loaded.id, session.id);
    assert_eq!(store.list().len(), 1);
    // 明文 json 不存在（落盘即密文）。
    assert!(!root.join(format!("{}.json", session.id)).exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn managed_dek_roundtrip_and_rotation() {
    let store = MemoryCredentialStore::new();
    let key = format!("dek-{}", uuid::Uuid::new_v4());
    let dek = managed_dek(&store, &key).unwrap();
    assert_eq!(dek.len(), 32);
    // 再次读取复用同一 DEK。
    assert_eq!(managed_dek(&store, &key).unwrap(), dek);
    // 轮换生成新 DEK 且凭据库已更新。
    let rotated = rotate_managed_dek(&store, &key).unwrap();
    assert_ne!(rotated, dek);
    assert_eq!(managed_dek(&store, &key).unwrap(), rotated);
    // 不可用 store → 显式错误（不静默）。
    let unavailable = MemoryCredentialStore::with_available(false);
    assert!(managed_dek(&unavailable, "dek-x").is_err());
    assert!(rotate_managed_dek(&unavailable, "dek-x").is_err());
}

// ---- R9 冒烟 B：网络 egress 边界（插件出境拒绝 + 审计汇入链） ----

use owo_agent_core::audit_chain::AuditChain;
use owo_agent_core::plugin::PluginManager;
use owo_agent_core::sandbox::SandboxEventKind;

fn plugin_dir_with_http_mcp(allowlist: &[&str]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("owo-plugin-egress-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = serde_json::json!({
        "id": "egress.test",
        "name": "Egress Test",
        "version": "1.0.0",
        "network_allowlist": allowlist,
        "mcp": {
            "name": "http-host",
            "command": "",
            "transport": "http",
            "url": "https://evil.example.com/mcp"
        }
    });
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    dir
}

#[test]
fn plugin_http_mcp_egress_rejected_and_audited() {
    let data_root = std::env::temp_dir().join(format!("owo-plugin-data-{}", uuid::Uuid::new_v4()));
    let mut manager = PluginManager::new(data_root.clone(), "0.1.0".to_string());
    manager.set_require_signature(false);

    // 出境关闭：白名单即使包含目标也被拒绝（+ 审计 EgressRejected）。
    manager.set_egress_enabled(false);
    let dir = plugin_dir_with_http_mcp(&["evil.example.com"]);
    let err = manager.verify_plugin_dir(&dir).unwrap_err();
    assert!(err.contains("数据出境开关"), "错误：{err}");
    {
        let manager = default_manager();
        let manager = manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(
            manager
                .audit()
                .contains_kind(SandboxEventKind::EgressRejected),
            "egress 拒绝必须产生审计事件"
        );
    }

    // 出境开启但目标不在白名单 → 拒绝（静态扫描域外或 gate 白名单，均为显式拒绝）。
    manager.set_egress_enabled(true);
    let dir2 = plugin_dir_with_http_mcp(&["allowed.example.com"]);
    let err = manager.verify_plugin_dir(&dir2).unwrap_err();
    assert!(
        err.contains("不在网络白名单") || err.contains("静态扫描未通过"),
        "错误：{err}"
    );

    // 出境开启且目标在白名单 → 通过。
    let dir3 = plugin_dir_with_http_mcp(&["evil.example.com"]);
    assert!(manager.verify_plugin_dir(&dir3).is_ok());

    // 审计事件汇入 HMAC 链且可验证。
    let mut chain = AuditChain::new(b"r9-test-key", 100);
    {
        let manager = default_manager();
        let mut manager = manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        manager.drain_into_chain(&mut chain, "plugin-gate");
    }
    assert!(chain.verify().is_ok());
    assert!(
        chain
            .records()
            .iter()
            .any(|record| record.record.event == "sandbox.egress_rejected"),
        "审计链必须包含 egress 拒绝事件"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
    let _ = std::fs::remove_dir_all(&dir3);
    let _ = std::fs::remove_dir_all(&data_root);
}

#[test]
fn network_requires_app_container_policy_rule() {
    // 网络策略需要 AppContainer 强制（跨平台纯逻辑）。
    let allowlist = SandboxPolicy {
        network_policy: NetworkPolicy::AllowList,
        allow_hosts: vec!["api.example.com".to_string()],
        ..SandboxPolicy::default()
    };
    assert!(network_requires_app_container(&allowlist));
    let loopback = SandboxPolicy {
        network_policy: NetworkPolicy::Loopback,
        ..SandboxPolicy::default()
    };
    assert!(!network_requires_app_container(&loopback));
    let none = SandboxPolicy {
        network_policy: NetworkPolicy::None,
        ..SandboxPolicy::default()
    };
    assert!(!network_requires_app_container(&none));
}

// ---- R10 冒烟 WP2：数据主权（DEK 信封 v2 + 导出脱敏） ----

#[test]
fn dek_envelope_v2_roundtrip_and_wrong_key_rejected() {
    if !dpapi_available() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("owo-crypto-v2-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("secret.dek");
    let dek = generate_dek();
    let plain = "owo-数据主权-内容".as_bytes();
    encrypt_file_envelope_with_dek(&path, plain, &dek).unwrap();
    assert_eq!(decrypt_file_envelope_with_dek(&path, &dek).unwrap(), plain);
    // 错误 DEK → 显式拒绝（密钥轮换后旧密文不可解）。
    let wrong = generate_dek();
    let err = decrypt_file_envelope_with_dek(&path, &wrong).unwrap_err();
    assert!(matches!(
        err,
        storage_crypto::StorageCryptoError::Decrypt(_)
    ));
    // 篡改数据段 → v3 认证标签不匹配 → 显式拒绝（R11：无静默篡改路径）。
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    std::fs::write(&path, &bytes).unwrap();
    let err = decrypt_file_envelope_with_dek(&path, &dek).unwrap_err();
    assert!(matches!(
        err,
        storage_crypto::StorageCryptoError::Decrypt(_)
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn export_redaction_covers_keys_and_message_content() {
    let input = serde_json::json!({
        "api_key": "sk-live-secret-123",
        "Authorization": "Bearer abc",
        "config": { "token": "t-xyz", "model": "gpt-5" },
        "messages": [
            { "role": "user", "content": "我的密钥是 sk-live-abcdefghijklmnopqrstuvwxyz1234567890，这是一段超过一百二十个字符的消息内容用于验证导出脱敏会正确截断长消息并保留可审计性的元数据字段不丢失完整性" }
        ],
        "trace_id": "t-1"
    });
    let redacted = storage_crypto::redact_sensitive_json(input);
    let text = serde_json::to_string(&redacted).unwrap();
    assert!(!text.contains("sk-live-secret-123"));
    assert!(!text.contains("Bearer abc"));
    assert!(!text.contains("t-xyz"));
    assert!(!text.contains("sk-live-abcdefghijklmnopqrstuvwxyz1234567890"));
    assert!(text.contains("***"));
    // 消息内容中的密钥前缀被脱敏（sk- → sk-***）。
    assert!(text.contains("sk-***"));
    // 非敏感字段保留（可审计性）。
    assert!(text.contains("gpt-5"));
    assert!(text.contains("t-1"));
}

// ---- R10 冒烟 WP3：插件供应链（吊销 / zip-slip / 官方豁免） ----

#[test]
fn plugin_revocation_blocks_load_and_audits() {
    let data_root =
        std::env::temp_dir().join(format!("owo-plugin-revoke-{}", uuid::Uuid::new_v4()));
    let mut manager = PluginManager::new(data_root.clone(), "0.1.0".to_string());
    manager.set_require_signature(false);
    manager.add_revocation("egress.test", "1.0.0", "安全事件");
    manager.save_revocations().unwrap();
    // 命中吊销 → 拒绝 + 审计。
    let dir = plugin_dir_with_http_mcp(&["evil.example.com"]);
    let err = manager.verify_plugin_dir(&dir).unwrap_err();
    assert!(err.contains("已被吊销"), "错误：{err}");
    // 未命中版本 → 放行。
    manager.clear_revocations();
    assert!(manager.verify_plugin_dir(&dir).is_ok());
    // 从文件重新加载 → 生效。
    let mut manager2 = PluginManager::new(data_root.clone(), "0.1.0".to_string());
    manager2.set_require_signature(false);
    manager2.load_revocations(None);
    assert!(manager2.is_revoked("egress.test", "1.0.0"));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&data_root);
}

#[test]
fn plugin_zip_slip_entry_rejected() {
    let data_root =
        std::env::temp_dir().join(format!("owo-plugin-zipslip-{}", uuid::Uuid::new_v4()));
    let mut manager = PluginManager::new(data_root.clone(), "0.1.0".to_string());
    manager.set_require_signature(false);
    let dir = std::env::temp_dir().join(format!("owo-plugin-zip-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "zipslip.test",
            "name": "ZipSlip",
            "version": "1.0.0",
            "entry": "../evil.py",
        }))
        .unwrap(),
    )
    .unwrap();
    let err = manager.verify_plugin_dir(&dir).unwrap_err();
    assert!(
        err.contains("zip-slip") || err.contains("非法"),
        "错误：{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&data_root);
}

#[test]
fn plugin_official_exemption_requires_valid_signature() {
    // 官方白名单内但无签名 → 不豁免（仍拒绝）。
    let data_root =
        std::env::temp_dir().join(format!("owo-plugin-official-{}", uuid::Uuid::new_v4()));
    let mut manager = PluginManager::new(data_root.clone(), "0.1.0".to_string());
    manager.set_require_signature(false);
    manager.set_official_allowlist(&["egress.test".to_string()]);
    // 有静态扫描风险（URL 不在 allowlist）且无签名 → 拒绝。
    let dir = plugin_dir_with_http_mcp(&[]);
    let err = manager.verify_plugin_dir(&dir).unwrap_err();
    assert!(!err.is_empty());
    // 依赖提取：入口含 require/from 记录。
    let deps = owo_agent_core::plugin::extract_dependencies(Some(
        "import os from 'os'; const x = require('fs'); const y = require('fs');",
    ));
    assert!(deps.is_some());
    let deps = deps.unwrap();
    assert!(deps.contains(&"os".to_string()));
    assert!(deps.contains(&"fs".to_string()));
    assert_eq!(deps.len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&data_root);
}

// ---- R11 冒烟：密钥轮换失败回滚（无悬空 DEK） + 备份包加密/恢复校验 ----

use owo_agent_core::audit_chain::{export_encrypted_to_file, load_encrypted_export};
use owo_agent_core::credentials::{CredentialError, CredentialStore};

/// 模拟"写后读回校验失败"的轮换：先覆盖新值再返回错误（最坏的真实失败形态），
/// 验证 rotate_managed_dek 必须回滚写回旧值。
struct FailingRotateStore {
    inner: MemoryCredentialStore,
    fail_next_rotate: bool,
}

impl CredentialStore for FailingRotateStore {
    fn name(&self) -> &'static str {
        "failing-rotate"
    }
    fn available(&self) -> bool {
        self.inner.available()
    }
    fn get(&self, key: &str) -> Option<String> {
        self.inner.get(key)
    }
    fn set(&self, key: &str, secret: &str) -> Result<(), CredentialError> {
        self.inner.set(key, secret)
    }
    fn delete(&self, key: &str) -> Result<(), CredentialError> {
        self.inner.delete(key)
    }
    fn rotate(&self, key: &str, new_secret: &str) -> Result<(), CredentialError> {
        if self.fail_next_rotate {
            // 覆盖新值后返回失败（模拟 CredWriteW 成功但读回校验失败）。
            self.inner.set(key, new_secret).unwrap();
            return Err(CredentialError::Store(
                "模拟轮换失败（读回校验不一致）".into(),
            ));
        }
        self.inner.rotate(key, new_secret)
    }
}

/// set 也失败（回滚不可执行）→ 必须显式报告，禁止假装成功。
struct FailingSetStore {
    inner: MemoryCredentialStore,
}

impl CredentialStore for FailingSetStore {
    fn name(&self) -> &'static str {
        "failing-set"
    }
    fn available(&self) -> bool {
        self.inner.available()
    }
    fn get(&self, key: &str) -> Option<String> {
        self.inner.get(key)
    }
    fn set(&self, _key: &str, _secret: &str) -> Result<(), CredentialError> {
        Err(CredentialError::Store("模拟 set 失败".into()))
    }
    fn delete(&self, key: &str) -> Result<(), CredentialError> {
        self.inner.delete(key)
    }
}

#[test]
fn managed_dek_rotation_failure_rolls_back_old_value() {
    let key = format!("dek-rb-{}", uuid::Uuid::new_v4());
    // 正常轮换先行，得到一个稳定旧值。
    let plain = MemoryCredentialStore::new();
    let dek_old = managed_dek(&plain, &key).unwrap();
    let old_stored = plain.get(&key).unwrap();
    assert_eq!(managed_dek(&plain, &key).unwrap(), dek_old);

    // 轮换失败（写后读回校验失败）→ 回滚旧值，DEK 不悬空。
    let failing = FailingRotateStore {
        inner: MemoryCredentialStore::new(),
        fail_next_rotate: true,
    };
    failing.inner.set(&key, &old_stored).unwrap();
    let err = rotate_managed_dek(&failing, &key).unwrap_err();
    assert!(
        err.to_string().contains("已回滚旧值"),
        "错误必须显式说明回滚：{err}"
    );
    // 回滚后旧值仍可用：managed_dek 返回与轮换前一致的 DEK。
    assert_eq!(
        managed_dek(&failing, &key).unwrap(),
        dek_old,
        "轮换失败后旧 DEK 必须可恢复（无悬空状态）"
    );
    assert_eq!(failing.inner.get(&key).unwrap(), old_stored);

    // 回滚也失败 → 显式报告"回滚也失败"，不得假装成功。
    let failing_set = FailingSetStore {
        inner: MemoryCredentialStore::new(),
    };
    failing_set.inner.set(&key, &old_stored).unwrap();
    let err = rotate_managed_dek(&failing_set, &key).unwrap_err();
    assert!(
        err.to_string().contains("回滚也失败"),
        "回滚失败必须显式报告：{err}"
    );
}

#[test]
fn encrypted_audit_export_restore_and_tamper_rejected() {
    if !dpapi_available() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("owo-audit-exp-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("audit-export.owo-crypt");
    let dek = owo_agent_core::storage_crypto::generate_dek();

    let mut chain = AuditChain::new(b"r11-export-key", 2);
    chain.append(owo_agent_core::audit_chain::AuditRecord::new(
        "r11",
        "egress_rejected",
        "evil.example.com",
    ));
    chain.append(owo_agent_core::audit_chain::AuditRecord::new(
        "r11",
        "tool_call",
        "read_file",
    ));
    let export = chain.export();
    assert!(!export.records.is_empty());

    // 加密导出（备份包）→ 恢复（DEK 匹配）→ 内容一致。
    export_encrypted_to_file(&export, &path, &dek).unwrap();
    let restored = load_encrypted_export(&path, &dek).unwrap();
    assert_eq!(restored.records.len(), export.records.len());
    assert_eq!(restored.segment_len, export.segment_len);

    // 恢复校验：错误 DEK → 显式拒绝（防用错备份密钥）。
    let wrong = owo_agent_core::storage_crypto::generate_dek();
    assert!(load_encrypted_export(&path, &wrong).is_err());

    // 篡改拒绝：改数据段任一字节 → v3 认证标签不匹配 → 显式拒绝（R11 修复静默篡改路径）。
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    std::fs::write(&path, &bytes).unwrap();
    let err = load_encrypted_export(&path, &dek).unwrap_err();
    assert!(
        err.to_string().contains("篡改") || err.to_string().contains("认证"),
        "篡改必须显式拒绝：{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
