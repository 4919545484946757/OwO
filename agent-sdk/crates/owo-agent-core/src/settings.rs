//! 工作区设置：`<workspace>/settings.json`（默认模型/只读/危险命令/MCP 服务器/v0.4 配置组）。

use crate::mcp::McpServerConfig;
use crate::whitelist::WhitelistEntry;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// 语音输入配置（v0.4 D20，默认 SenseVoice-Small 本地转写）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttSettings {
    #[serde(default = "default_stt_model")]
    pub model: String,
    /// SenseVoice 语言（auto / zh / en / ja / ko / yue），可用 OWO_STT_LANGUAGE 覆盖。
    #[serde(default = "default_stt_language")]
    pub language: String,
    /// 是否启用逆文本规范化（ITN），可用 OWO_STT_ITN 覆盖。
    #[serde(default = "default_true")]
    pub itn: bool,
    #[serde(default = "default_false")]
    pub enable_high_accuracy: bool,
    #[serde(default)]
    pub hotwords: Vec<String>,
    #[serde(default = "default_latency_budget")]
    pub latency_budget_ms: u64,
}

impl Default for SttSettings {
    fn default() -> Self {
        Self {
            model: "SenseVoice-Small".to_string(),
            language: "auto".to_string(),
            itn: true,
            enable_high_accuracy: false,
            hotwords: Vec::new(),
            latency_budget_ms: 2000,
        }
    }
}

/// 受限自主探索配置（v0.4 D23，默认 S0 隔离虚拟机层）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExploreSettings {
    #[serde(default = "default_explore_tier")]
    pub default_tier: String,
    #[serde(default = "default_action_budget")]
    pub action_budget: u32,
    #[serde(default = "default_max_duration")]
    pub max_duration_s: u64,
    #[serde(default = "default_false")]
    pub allow_s1: bool,
}

impl Default for ExploreSettings {
    fn default() -> Self {
        Self {
            default_tier: "S0".to_string(),
            action_budget: 50,
            max_duration_s: 600,
            allow_s1: false,
        }
    }
}

/// 主动建议阈值配置（v0.4 D24，默认仅提示不执行）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProactiveSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_weekly_threshold")]
    pub weekly_threshold: u32,
    #[serde(default = "default_daily_threshold")]
    pub daily_threshold: u32,
    #[serde(default = "default_similarity")]
    pub similarity: f64,
    #[serde(default = "default_cooldown_hours")]
    pub cooldown_hours: u32,
    #[serde(default = "default_daily_cap")]
    pub daily_cap: u32,
    #[serde(default = "default_auto_silence_days")]
    pub auto_silence_days: u32,
}

impl Default for ProactiveSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            weekly_threshold: 5,
            daily_threshold: 3,
            similarity: 0.9,
            cooldown_hours: 24,
            daily_cap: 3,
            auto_silence_days: 30,
        }
    }
}

/// 技能包分享/导入配置（v0.4 D26）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillsSettings {
    #[serde(default = "default_share_format")]
    pub share_format: String,
    #[serde(default = "default_false")]
    pub require_signature: bool,
}

fn default_stt_model() -> String {
    "SenseVoice-Small".to_string()
}

fn default_stt_language() -> String {
    "auto".to_string()
}

fn default_false() -> bool {
    false
}

fn default_true() -> bool {
    true
}

fn default_latency_budget() -> u64 {
    2000
}

fn default_explore_tier() -> String {
    "S0".to_string()
}

fn default_action_budget() -> u32 {
    50
}

fn default_max_duration() -> u64 {
    600
}

fn default_weekly_threshold() -> u32 {
    5
}

fn default_daily_threshold() -> u32 {
    3
}

fn default_similarity() -> f64 {
    0.9
}

fn default_cooldown_hours() -> u32 {
    24
}

fn default_daily_cap() -> u32 {
    3
}

fn default_auto_silence_days() -> u32 {
    30
}

fn default_share_format() -> String {
    "owskill".to_string()
}

impl Default for SkillsSettings {
    fn default() -> Self {
        Self {
            share_format: "owskill".to_string(),
            require_signature: false,
        }
    }
}

/// 数据出境开关（v0.3 7.5）：关闭后拒绝云端模型调用。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressSettings {
    #[serde(default = "default_true")]
    pub cloud_enabled: bool,
}

impl Default for EgressSettings {
    fn default() -> Self {
        Self {
            cloud_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
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
    /// TUI 主题：dark / light。
    #[serde(default)]
    pub theme: Option<String>,
    /// TUI 键位：action → 按键描述（如 "tab"、"ctrl+c"、"f2"）。
    #[serde(default)]
    pub keybinds: HashMap<String, String>,
    /// v0.4 语音输入配置。
    #[serde(default)]
    pub stt: SttSettings,
    /// v0.4 自主探索配置。
    #[serde(default)]
    pub explore: ExploreSettings,
    /// v0.4 主动建议配置。
    #[serde(default)]
    pub proactive: ProactiveSettings,
    /// v0.4 技能包分享/导入配置。
    #[serde(default)]
    pub skills: SkillsSettings,
    /// v0.4 应用白名单（可被默认清单覆盖，用户增删）。
    #[serde(default)]
    pub whitelist: Vec<WhitelistEntry>,
    /// 数据出境开关。
    #[serde(default)]
    pub egress: EgressSettings,
}

impl Settings {
    pub fn load(workspace: &Path) -> Self {
        let path = workspace.join("settings.json");
        std::fs::read_to_string(path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, workspace: &Path) -> Result<(), String> {
        let path = workspace.join("settings.json");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(self).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())
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
                "model": "deepseek-v4-flash",
                "read_only": true,
                "deny_commands": ["git push"],
                "mcp_servers": [
                    { "name": "files", "transport": "stdio", "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "."] }
                ],
                "theme": "light",
                "keybinds": { "toggle_mode": "f2" },
                "stt": { "model": "SenseVoice-Small", "hotwords": ["VSCode", "提交"] },
                "explore": { "default_tier": "S0", "action_budget": 20 },
                "proactive": { "enabled": true, "weekly_threshold": 3 },
                "skills": { "share_format": "owskill" },
                "whitelist": [
                    { "app_id": "code", "name": "VSCode", "tier": "productivity", "learn_allowed": true, "auto_ops_allowed": true }
                ]
            }"#,
        )
        .unwrap();
        let settings = Settings::load(&workspace);
        assert_eq!(settings.model.as_deref(), Some("deepseek-v4-flash"));
        assert!(settings.read_only);
        assert_eq!(settings.deny_commands, vec!["git push"]);
        assert_eq!(settings.mcp_servers.len(), 1);
        assert_eq!(settings.mcp_servers[0].name, "files");
        assert_eq!(settings.theme.as_deref(), Some("light"));
        assert_eq!(
            settings.keybinds.get("toggle_mode").map(String::as_str),
            Some("f2")
        );
        assert_eq!(settings.stt.model, "SenseVoice-Small");
        assert_eq!(settings.stt.hotwords, vec!["VSCode", "提交"]);
        assert_eq!(settings.explore.default_tier, "S0");
        assert_eq!(settings.explore.action_budget, 20);
        assert_eq!(settings.proactive.weekly_threshold, 3);
        assert_eq!(settings.proactive.daily_threshold, 3);
        assert_eq!(settings.skills.share_format, "owskill");
        assert_eq!(settings.whitelist.len(), 1);
        assert_eq!(settings.whitelist[0].app_id, "code");
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
        assert!(settings.theme.is_none());
        assert!(settings.keybinds.is_empty());
        assert_eq!(settings.stt.model, "SenseVoice-Small");
        assert!(!settings.stt.enable_high_accuracy);
        assert_eq!(settings.explore.default_tier, "S0");
        assert_eq!(settings.explore.action_budget, 50);
        assert!(!settings.explore.allow_s1);
        assert!(settings.proactive.enabled);
        assert_eq!(settings.proactive.weekly_threshold, 5);
        assert_eq!(settings.proactive.similarity, 0.9);
        assert_eq!(settings.skills.share_format, "owskill");
        assert!(!settings.skills.require_signature);
        assert!(settings.whitelist.is_empty());
        assert!(settings.egress.cloud_enabled);
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn save_and_load_round_trip_preserves_all_groups() {
        let workspace =
            std::env::temp_dir().join(format!("owo-settings-save-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();
        let settings = Settings {
            model: Some("deepseek-v4-flash".to_string()),
            read_only: true,
            stt: SttSettings {
                model: "Other-Model".to_string(),
                language: "zh".to_string(),
                itn: false,
                ..SttSettings::default()
            },
            proactive: ProactiveSettings {
                enabled: false,
                ..ProactiveSettings::default()
            },
            egress: EgressSettings {
                cloud_enabled: false,
            },
            ..Settings::default()
        };
        settings.save(&workspace).unwrap();
        let loaded = Settings::load(&workspace);
        assert_eq!(loaded.model.as_deref(), Some("deepseek-v4-flash"));
        assert!(loaded.read_only);
        assert_eq!(loaded.stt.model, "Other-Model");
        assert_eq!(loaded.stt.language, "zh");
        assert!(!loaded.stt.itn);
        assert!(!loaded.proactive.enabled);
        assert!(!loaded.egress.cloud_enabled);
        let _ = std::fs::remove_dir_all(&workspace);
    }
}
