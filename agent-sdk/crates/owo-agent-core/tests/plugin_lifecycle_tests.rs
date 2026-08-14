use owo_agent_core::plugin::{discover_enabled_plugins, discover_plugins, PluginStateStore};
use std::path::Path;

fn write_plugin(root: &Path, id: &str, name: &str) {
    let dir = root.join("plugins").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = format!(r#"{{"id":"{id}","name":"{name}","version":"1.0.0"}}"#);
    std::fs::write(dir.join("manifest.json"), manifest).unwrap();
}

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
