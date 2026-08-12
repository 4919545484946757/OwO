//! 应用白名单（v0.4 D25）：生产力 P0 / 聊天类 P1 / 游戏只读 / 其他只读辅助。
//!
//! 白名单决定感知层级、可操作性与学习权限；敏感类应用（支付/密码管理器）
//! 默认禁止自动操作与学习。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppTier {
    Productivity,
    Chat,
    Game,
    Other,
}

impl AppTier {
    pub fn label(&self) -> &'static str {
        match self {
            AppTier::Productivity => "生产力 P0",
            AppTier::Chat => "聊天类 P1",
            AppTier::Game => "游戏（只读）",
            AppTier::Other => "其他（只读辅助）",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhitelistEntry {
    pub app_id: String,
    pub name: String,
    pub tier: AppTier,
    /// 聊天类应用：会话级授权后才可读取消息内容。
    #[serde(default)]
    pub chat_authorized: bool,
    /// 是否允许沉淀操作流程技能（聊天类不学消息内容，只学操作流程）。
    #[serde(default)]
    pub learn_allowed: bool,
    /// 是否允许审批后自动操作（游戏类一律 false）。
    #[serde(default)]
    pub auto_ops_allowed: bool,
    /// 敏感类（支付/密码管理器）：默认禁止自动操作与学习。
    #[serde(default)]
    pub sensitive: bool,
}

#[derive(Debug, Clone)]
pub struct Whitelist {
    entries: Vec<WhitelistEntry>,
}

impl Whitelist {
    pub fn default_entries() -> Vec<WhitelistEntry> {
        fn entry(
            app_id: &str,
            name: &str,
            tier: AppTier,
            learn: bool,
            ops: bool,
        ) -> WhitelistEntry {
            WhitelistEntry {
                app_id: app_id.to_string(),
                name: name.to_string(),
                tier,
                chat_authorized: false,
                learn_allowed: learn,
                auto_ops_allowed: ops,
                sensitive: false,
            }
        }
        vec![
            entry("code", "VSCode", AppTier::Productivity, true, true),
            entry("cursor", "Cursor", AppTier::Productivity, true, true),
            entry("chrome", "Chrome", AppTier::Productivity, true, true),
            entry("edge", "Edge", AppTier::Productivity, true, true),
            entry("firefox", "Firefox", AppTier::Productivity, true, true),
            entry(
                "explorer",
                "文件资源管理器",
                AppTier::Productivity,
                true,
                true,
            ),
            entry("terminal", "终端", AppTier::Productivity, true, true),
            entry("word", "Word", AppTier::Productivity, true, true),
            entry("excel", "Excel", AppTier::Productivity, true, true),
            entry(
                "powerpoint",
                "PowerPoint",
                AppTier::Productivity,
                true,
                true,
            ),
            entry("qq", "QQ", AppTier::Chat, true, true),
            entry("wechat", "微信", AppTier::Chat, true, true),
            entry("feishu", "飞书", AppTier::Chat, true, true),
            WhitelistEntry {
                app_id: "password-manager".to_string(),
                name: "密码管理器".to_string(),
                tier: AppTier::Other,
                chat_authorized: false,
                learn_allowed: false,
                auto_ops_allowed: false,
                sensitive: true,
            },
            WhitelistEntry {
                app_id: "payment".to_string(),
                name: "支付类应用".to_string(),
                tier: AppTier::Other,
                chat_authorized: false,
                learn_allowed: false,
                auto_ops_allowed: false,
                sensitive: true,
            },
        ]
    }

    pub fn new(entries: Vec<WhitelistEntry>) -> Self {
        Self { entries }
    }

    pub fn entries(&self) -> &[WhitelistEntry] {
        &self.entries
    }

    pub fn get(&self, app_id: &str) -> Option<&WhitelistEntry> {
        self.entries.iter().find(|entry| entry.app_id == app_id)
    }

    /// 新增或替换；返回是否新增。
    pub fn upsert(&mut self, entry: WhitelistEntry) -> bool {
        if let Some(existing) = self.entries.iter_mut().find(|e| e.app_id == entry.app_id) {
            *existing = entry;
            false
        } else {
            self.entries.push(entry);
            true
        }
    }

    pub fn remove(&mut self, app_id: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.app_id != app_id);
        self.entries.len() != before
    }

    pub fn tier_for(&self, app_id: &str) -> AppTier {
        self.get(app_id)
            .map(|entry| entry.tier)
            .unwrap_or(AppTier::Other)
    }

    pub fn is_sensitive(&self, app_id: &str) -> bool {
        self.get(app_id)
            .map(|entry| entry.sensitive)
            .unwrap_or(false)
    }

    /// 是否允许审批后操作：生产力 + 允许自动操作，或聊天类 + 会话授权。
    pub fn can_operate(&self, app_id: &str) -> bool {
        match self.get(app_id) {
            Some(entry) if !entry.sensitive => {
                (entry.tier == AppTier::Productivity && entry.auto_ops_allowed)
                    || (entry.tier == AppTier::Chat && entry.chat_authorized)
            }
            _ => false,
        }
    }

    /// 是否允许学习操作流程（游戏与敏感类一律不允许）。
    pub fn can_learn(&self, app_id: &str) -> bool {
        self.get(app_id)
            .map(|entry| !entry.sensitive && entry.learn_allowed)
            .unwrap_or(false)
    }

    /// 按前台窗口启发式分类：白名单外的全屏窗口视为游戏（只读）。
    pub fn classify(&self, app_id: &str, fullscreen: bool) -> AppTier {
        match self.tier_for(app_id) {
            AppTier::Other if fullscreen => AppTier::Game,
            tier => tier,
        }
    }
}

impl Default for Whitelist {
    fn default() -> Self {
        Self::new(Self::default_entries())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_cover_productivity_chat_and_sensitive() {
        let whitelist = Whitelist::default();
        assert_eq!(whitelist.tier_for("code"), AppTier::Productivity);
        assert_eq!(whitelist.tier_for("qq"), AppTier::Chat);
        assert!(whitelist.can_operate("code"));
        assert!(!whitelist.can_operate("qq")); // 未会话授权
        assert!(whitelist.can_learn("qq"));
        assert!(whitelist.is_sensitive("password-manager"));
        assert!(!whitelist.can_operate("payment"));
        assert!(!whitelist.can_learn("password-manager"));
    }

    #[test]
    fn upsert_and_remove_manage_entries() {
        let mut whitelist = Whitelist::default();
        assert!(whitelist.upsert(WhitelistEntry {
            app_id: "custom-app".to_string(),
            name: "自定义".to_string(),
            tier: AppTier::Productivity,
            chat_authorized: false,
            learn_allowed: true,
            auto_ops_allowed: true,
            sensitive: false,
        }));
        assert_eq!(whitelist.tier_for("custom-app"), AppTier::Productivity);
        assert!(!whitelist.upsert(WhitelistEntry {
            app_id: "custom-app".to_string(),
            name: "自定义 2".to_string(),
            tier: AppTier::Other,
            chat_authorized: false,
            learn_allowed: false,
            auto_ops_allowed: false,
            sensitive: false,
        }));
        assert_eq!(whitelist.get("custom-app").unwrap().name, "自定义 2");
        assert!(whitelist.remove("custom-app"));
        assert!(!whitelist.remove("custom-app"));
    }

    #[test]
    fn fullscreen_unlisted_apps_classify_as_game() {
        let whitelist = Whitelist::default();
        assert_eq!(whitelist.classify("some-game", true), AppTier::Game);
        assert_eq!(whitelist.classify("some-app", false), AppTier::Other);
        assert_eq!(whitelist.classify("code", true), AppTier::Productivity);
    }
}
