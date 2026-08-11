//! 工作区设置：`<workspace>/settings.json`（默认模型/只读/危险命令/MCP 服务器）。

use crate::mcp::McpServerConfig;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    /// 默认模型（低于环境变量与命令行参数）。
    #[serde(default)]
    pub model: Option<String>,
    /// 启动默认只读（plan）模式。
    #[serde(default)]
    pub read_only: bool,
    /// 额外危险命令片段（deny 优先）。
    #[serde(default)]
    pub deny_commands: Vec<String>,
    /// 启动时自动连接的 MCP 服务器。
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
}

impl Settings {
    pub fn load(workspace: &Path) -> Self {
        let path = workspace.join("settings.json");
        std::fs::read_to_string(path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_settings_from_workspace() {
        let workspace =
            std::env::temp_dir().join(format!("owo-settings-workspace-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            workspace.join("settings.json"),
            r#"{
                "model": "deepseek-v4-pro",
                "read_only": true,
                "deny_commands": ["git push"],
                "mcp_servers": [
                    { "name": "files", "transport": "stdio", "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "."] }
                ]
            }"#,
        )
        .unwrap();
        let settings = Settings::load(&workspace);
        assert_eq!(settings.model.as_deref(), Some("deepseek-v4-pro"));
        assert!(settings.read_only);
        assert_eq!(settings.deny_commands, vec!["git push"]);
        assert_eq!(settings.mcp_servers.len(), 1);
        assert_eq!(settings.mcp_servers[0].name, "files");
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn missing_settings_returns_defaults() {
        let workspace =
            std::env::temp_dir().join(format!("owo-settings-missing-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();
        let settings = Settings::load(&workspace);
        assert!(settings.model.is_none());
        assert!(!settings.read_only);
        assert!(settings.deny_commands.is_empty());
        let _ = std::fs::remove_dir_all(&workspace);
    }
}
