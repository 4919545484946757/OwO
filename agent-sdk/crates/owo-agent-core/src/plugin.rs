//! 本地插件：manifest 解析与发现（工具经 MCP 服务器桥接）。

use crate::mcp::McpServerConfig;
use serde::{Deserialize, Serialize};
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
}
