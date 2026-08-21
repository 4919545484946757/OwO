//! credentials.rs 契约测试（X02）：引用型凭据、来源优先级、
//! 凭据库抽象、settings.json 零明文契约（Wave 1）+ Windows Credential Manager
//! 读写删闭环（Wave 2，能力不可用时显式跳过而非假装通过）。

use owo_agent_core::credentials::*;
use std::sync::Mutex;

/// 环境变量测试串行化（避免并行污染）。
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn with_env(var: &str, value: &str, test: impl FnOnce()) {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::env::set_var(var, value);
    test();
    std::env::remove_var(var);
}

/// Windows Credential Manager 可用性门控：不可用时显式跳过（打印原因）。
fn wcm_store() -> Option<Box<dyn CredentialStore>> {
    let store = windows_credential_manager();
    if !store.available() {
        eprintln!("SKIP: Windows Credential Manager 不可用，测试显式跳过（probe failed）");
        return None;
    }
    Some(store)
}

#[test]
fn env_ref_resolves_from_environment() {
    with_env("OWO_TEST_API_KEY", "sk-env-secret", || {
        let resolver = CredentialResolver::no_store();
        let api_key_ref = ApiKeyRef::from_env("OWO_TEST_API_KEY");
        assert_eq!(resolver.resolve(&api_key_ref).unwrap(), "sk-env-secret");
    });
}

#[test]
fn store_takes_priority_over_environment() {
    with_env("OWO_TEST_API_KEY", "sk-env-secret", || {
        let store = MemoryCredentialStore::seeded(&[("my-openai", "sk-store-secret")]);
        let resolver = CredentialResolver::new(Box::new(store));
        let api_key_ref = ApiKeyRef {
            store_key: Some("my-openai".to_string()),
            env_var: Some("OWO_TEST_API_KEY".to_string()),
            ..ApiKeyRef::default()
        };
        assert_eq!(resolver.resolve(&api_key_ref).unwrap(), "sk-store-secret");
    });
}

#[test]
fn store_missing_falls_back_to_environment() {
    with_env("OWO_TEST_API_KEY", "sk-env-secret", || {
        let store = MemoryCredentialStore::new();
        let resolver = CredentialResolver::new(Box::new(store));
        let api_key_ref = ApiKeyRef {
            store_key: Some("not-exist".to_string()),
            env_var: Some("OWO_TEST_API_KEY".to_string()),
            ..ApiKeyRef::default()
        };
        assert_eq!(resolver.resolve(&api_key_ref).unwrap(), "sk-env-secret");
    });
}

#[test]
fn missing_everything_returns_explicit_error() {
    with_env("OWO_TEST_API_KEY", "x", || {
        std::env::remove_var("OWO_TEST_API_KEY");
        let resolver = CredentialResolver::no_store();
        let api_key_ref = ApiKeyRef::from_env("OWO_TEST_API_KEY");
        match resolver.resolve(&api_key_ref) {
            Err(CredentialError::Missing(_)) => {}
            other => panic!("必须显式 Missing，实际 {:?}", other),
        }
    });
}

#[test]
fn empty_env_value_treated_as_missing() {
    with_env("OWO_TEST_API_KEY", "", || {
        let resolver = CredentialResolver::no_store();
        let api_key_ref = ApiKeyRef::from_env("OWO_TEST_API_KEY");
        assert!(resolver.resolve(&api_key_ref).is_err());
    });
}

#[test]
fn inline_never_serialized_into_json() {
    let config = ProviderConfig {
        name: "openai".to_string(),
        api_key_ref: Some(ApiKeyRef::inline("sk-super-secret-inline")),
        base_url: None,
        model: None,
    };
    let json = config.serialized_without_plaintext().unwrap();
    assert!(!json.contains("sk-super-secret-inline"));
    assert!(json.contains("api_key_ref"));
}

#[test]
fn settings_json_plaintext_scan_contract() {
    // 契约：settings.json 永不出现明文密钥——落盘 JSON 必须通过明文扫描。
    let config = ProviderConfig::with_store_key("openai", "my-openai");
    let json = config.serialized_without_plaintext().unwrap();
    assert!(scan_json_for_secrets(&json, &["sk-live-secret", "sk-super-secret-inline"]).is_empty());

    // 反例：若明文混入，扫描必须能检出（门禁用）。
    let leaked = r#"{"api_key":"sk-live-secret"}"#;
    assert_eq!(
        scan_json_for_secrets(leaked, &["sk-live-secret"]),
        vec!["sk-live-secret".to_string()]
    );
}

#[test]
fn memory_store_set_get_delete() {
    let store = MemoryCredentialStore::new();
    assert!(store.available());
    store.set("k1", "v1").unwrap();
    assert_eq!(store.get("k1").as_deref(), Some("v1"));
    store.delete("k1").unwrap();
    assert_eq!(store.get("k1"), None);
}

#[test]
fn unavailable_store_is_explicit_not_silent() {
    let store = UnavailableStore {
        reason: "未接入"
    };
    assert!(!store.available());
    assert!(store.set("k", "v").is_err());
    assert!(store.delete("k").is_err());
}

#[test]
fn provider_config_round_trip_preserves_refs_not_secrets() {
    let config = ProviderConfig {
        name: "openai".to_string(),
        api_key_ref: Some(ApiKeyRef {
            store_key: Some("my-openai".to_string()),
            env_var: Some("OPENAI_API_KEY".to_string()),
            inline: Some("sk-inline-test".to_string()),
        }),
        base_url: Some("https://api.openai.com/v1".to_string()),
        model: Some("gpt-5".to_string()),
    };
    let json = config.serialized_without_plaintext().unwrap();
    assert!(!json.contains("sk-inline-test"));
    let loaded: ProviderConfig = serde_json::from_str(&json).unwrap();
    let refs = loaded.api_key_ref.unwrap();
    assert_eq!(refs.store_key.as_deref(), Some("my-openai"));
    assert_eq!(refs.env_var.as_deref(), Some("OPENAI_API_KEY"));
    assert!(refs.inline.is_none());
    assert_eq!(
        loaded.base_url.as_deref(),
        Some("https://api.openai.com/v1")
    );
    assert_eq!(loaded.model.as_deref(), Some("gpt-5"));
}

#[test]
fn has_refs_reports_usable_references() {
    assert!(ApiKeyRef::from_env("X").has_refs());
    assert!(ApiKeyRef::from_store("k").has_refs());
    assert!(!ApiKeyRef::default().has_refs());
    assert!(!ApiKeyRef::inline("s").has_refs());
}

#[test]
fn wcm_write_read_delete_roundtrip() {
    let Some(store) = wcm_store() else {
        return;
    };
    let key = format!("test-roundtrip-{}", uuid::Uuid::new_v4());
    let secret = "sk-credential-manager-秘密-123";
    store.set(&key, secret).unwrap();
    assert_eq!(store.get(&key).as_deref(), Some(secret));
    store.delete(&key).unwrap();
    assert_eq!(store.get(&key), None);
}

#[test]
fn wcm_missing_entry_returns_none() {
    let Some(store) = wcm_store() else {
        return;
    };
    let key = format!("test-missing-{}", uuid::Uuid::new_v4());
    assert_eq!(store.get(&key), None);
    // 删除不存在的条目：显式错误而非静默成功。
    assert!(store.delete(&key).is_err());
}

#[test]
fn wcm_roundtrip_preserves_utf8_blob() {
    let Some(store) = wcm_store() else {
        return;
    };
    let key = format!("test-utf8-{}", uuid::Uuid::new_v4());
    let secret = "多行\n密钥内容 ~!@#$%^&*()_+";
    store.set(&key, secret).unwrap();
    assert_eq!(store.get(&key).as_deref(), Some(secret));
    store.delete(&key).unwrap();
}

#[test]
fn wcm_resolver_uses_store_before_env() {
    let Some(store) = wcm_store() else {
        return;
    };
    let key = format!("test-resolver-{}", uuid::Uuid::new_v4());
    store.set(&key, "sk-from-store").unwrap();
    let resolver = CredentialResolver::new(store);
    with_env("OWO_TEST_API_KEY", "sk-from-env", || {
        let api_key_ref = ApiKeyRef {
            store_key: Some(key.clone()),
            env_var: Some("OWO_TEST_API_KEY".to_string()),
            ..ApiKeyRef::default()
        };
        assert_eq!(resolver.resolve(&api_key_ref).unwrap(), "sk-from-store");
    });
    resolver.store().delete(&key).unwrap();
}

#[test]
fn wcm_settings_json_never_leaks_plaintext() {
    let Some(store) = wcm_store() else {
        return;
    };
    let key = format!("test-settings-{}", uuid::Uuid::new_v4());
    store.set(&key, "sk-super-secret-wcm").unwrap();
    let config = ProviderConfig {
        name: "openai".to_string(),
        api_key_ref: Some(ApiKeyRef {
            store_key: Some(key.clone()),
            env_var: Some("OPENAI_API_KEY".to_string()),
            ..ApiKeyRef::default()
        }),
        base_url: None,
        model: None,
    };
    let json = config.serialized_without_plaintext().unwrap();
    assert!(!json.contains("sk-super-secret-wcm"));
    assert!(scan_json_for_secrets(&json, &["sk-super-secret-wcm"]).is_empty());
    store.delete(&key).unwrap();
}
