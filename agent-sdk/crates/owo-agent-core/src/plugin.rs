//! 本地插件：manifest 解析与发现（工具经 MCP 服务器桥接）。

use crate::mcp::McpServerConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    /// 插件提供的 MCP 服务器（工具桥接）。
    #[serde(default)]
    pub mcp: Option<McpServerConfig>,
}

impl PluginManifest {
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
        let manifest: PluginManifest = serde_json::from_str(&content)
            .map_err(|error| format!("manifest 解析失败：{error}"))?;
        if manifest.id.trim().is_empty() || manifest.name.trim().is_empty() {
            return Err("插件 id 与 name 不能为空".to_string());
        }
        Ok(manifest)
    }
}

/// 从全局 `<data>/plugins` 与工作区 `plugins/` 发现插件（按 id 去重，工作区优先）。
pub fn discover_plugins(workspace: &Path, data_root: &Path) -> Vec<(PathBuf, PluginManifest)> {
    let mut plugins = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for root in [workspace.join("plugins"), data_root.join("plugins")] {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
            if !is_dir {
                continue;
            }
            let manifest_path = entry.path().join("manifest.json");
            if !manifest_path.exists() {
                continue;
            }
            if let Ok(manifest) = PluginManifest::load(&manifest_path) {
                if seen.insert(manifest.id.clone()) {
                    plugins.push((manifest_path, manifest));
                }
            }
        }
    }
    plugins
}

/// 插件启用状态存储（v0.5 生产加固）：只记录被禁用的 id，
/// 因此未声明过的插件默认启用，状态文件可延迟创建。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginStateStore {
    /// 已禁用的插件 id。
    disabled: HashSet<String>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl PluginStateStore {
    /// 绑定持久化路径（如 `<data>/plugin_state.json`）；None 表示纯内存。
    pub fn new(path: Option<PathBuf>) -> Self {
        let mut store = Self {
            disabled: HashSet::new(),
            path,
        };
        if let Some(path) = &store.path {
            store.disabled = Self::load(path);
        }
        store
    }

    fn load(path: &Path) -> HashSet<String> {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|content| serde_json::from_str::<PluginStateStore>(&content).ok())
            .map(|store| store.disabled)
            .unwrap_or_default()
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), String> {
        if enabled {
            self.disabled.remove(id);
        } else {
            self.disabled.insert(id.to_string());
        }
        self.save()
    }

    /// 未记录的插件默认启用。
    pub fn is_enabled(&self, id: &str) -> bool {
        !self.disabled.contains(id)
    }

    pub fn disabled_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.disabled.iter().cloned().collect();
        ids.sort();
        ids
    }

    /// 恢复全部插件为启用状态。
    pub fn reset(&mut self) -> Result<(), String> {
        self.disabled.clear();
        self.save()
    }

    fn save(&self) -> Result<(), String> {
        if let Some(path) = &self.path {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            let json = serde_json::to_string_pretty(&self).map_err(|error| error.to_string())?;
            std::fs::write(path, json).map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

/// 只返回启用插件的发现结果（配合 `PluginStateStore::is_enabled`）。
pub fn discover_enabled_plugins(
    workspace: &Path,
    data_root: &Path,
    state: &PluginStateStore,
) -> Vec<(PathBuf, PluginManifest)> {
    discover_plugins(workspace, data_root)
        .into_iter()
        .filter(|(_, manifest)| state.is_enabled(&manifest.id))
        .collect()
}

/// 插件 → MCP 服务器配置：以插件 id 为服务器名，相对启动命令按 manifest 所在目录解析。
/// 无 `mcp` 声明返回 None。
pub fn plugin_mcp_config(
    manifest_path: &Path,
    manifest: &PluginManifest,
) -> Option<McpServerConfig> {
    let mut config = manifest.mcp.clone()?;
    config.name = manifest.id.clone();
    let command_path = Path::new(&config.command);
    if command_path.is_relative() {
        if let Some(base) = manifest_path.parent() {
            let resolved = base.join(command_path);
            if resolved.exists() {
                config.command = resolved.to_string_lossy().into_owned();
            }
        }
    }
    if let Some(base) = manifest_path.parent() {
        config.args = config
            .args
            .into_iter()
            .map(|argument| {
                let path = Path::new(&argument);
                if path.is_relative() {
                    let resolved = base.join(path);
                    if resolved.exists() {
                        return resolved.to_string_lossy().into_owned();
                    }
                }
                argument
            })
            .collect();
    }
    Some(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_and_validates_manifest() {
        let dir =
            std::env::temp_dir().join(format!("owo-plugin-manifest-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("manifest.json");
        std::fs::write(
            &path,
            r#"{
                "id": "owo.plugin.demo",
                "name": "Demo",
                "version": "1.0.0",
                "permissions": ["agent:tools"],
                "mcp": {
                    "name": "demo",
                    "transport": "stdio",
                    "command": "demo-server",
                    "args": []
                }
            }"#,
        )
        .unwrap();
        let manifest = PluginManifest::load(&path).unwrap();
        assert_eq!(manifest.id, "owo.plugin.demo");
        assert!(manifest.mcp.is_some());

        std::fs::write(&path, r#"{"id":"","name":"","version":"1"}"#).unwrap();
        assert!(PluginManifest::load(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discovers_plugins_with_workspace_precedence() {
        let workspace =
            std::env::temp_dir().join(format!("owo-plugin-workspace-{}", uuid::Uuid::new_v4()));
        let data = std::env::temp_dir().join(format!("owo-plugin-data-{}", uuid::Uuid::new_v4()));
        let dir = workspace.join("plugins").join("a");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            r#"{"id":"a","name":"A","version":"1.0.0"}"#,
        )
        .unwrap();
        let global = data.join("plugins").join("a");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(
            global.join("manifest.json"),
            r#"{"id":"a","name":"A-global","version":"1.0.0"}"#,
        )
        .unwrap();
        let other = data.join("plugins").join("b");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(
            other.join("manifest.json"),
            r#"{"id":"b","name":"B","version":"2.0.0"}"#,
        )
        .unwrap();

        let plugins = discover_plugins(&workspace, &data);
        assert_eq!(plugins.len(), 2);
        let a = plugins.iter().find(|(_, m)| m.id == "a").unwrap();
        assert_eq!(a.1.name, "A"); // 工作区优先
        let _ = std::fs::remove_dir_all(&workspace);
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn plugin_mcp_config_resolves_relative_command() {
        let dir = std::env::temp_dir().join(format!("owo-plugin-mcp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("server.py"), "# placeholder").unwrap();
        std::fs::write(dir.join("worker.py"), "# placeholder").unwrap();
        let manifest_path = dir.join("manifest.json");
        let manifest = PluginManifest {
            id: "owo.demo".to_string(),
            name: "Demo".to_string(),
            version: "1.0.0".to_string(),
            description: String::new(),
            permissions: Vec::new(),
            mcp: Some(McpServerConfig {
                name: "ignored".to_string(),
                transport: "stdio".to_string(),
                command: "server.py".to_string(),
                args: vec!["worker.py".to_string(), "--stdio".to_string()],
                url: None,
                timeout_ms: None,
            }),
        };
        // 相对命令按 manifest 目录解析；服务器名 = 插件 id。
        let config = plugin_mcp_config(&manifest_path, &manifest).expect("应产出 MCP 配置");
        assert_eq!(config.name, "owo.demo");
        assert_eq!(config.command, dir.join("server.py").to_string_lossy());
        assert_eq!(config.args[0], dir.join("worker.py").to_string_lossy());
        assert_eq!(config.args[1], "--stdio");
        // 无 mcp 声明返回 None。
        let bare = PluginManifest {
            id: "owo.view".to_string(),
            name: "View".to_string(),
            version: "1.0.0".to_string(),
            description: String::new(),
            permissions: Vec::new(),
            mcp: None,
        };
        assert!(plugin_mcp_config(&manifest_path, &bare).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
