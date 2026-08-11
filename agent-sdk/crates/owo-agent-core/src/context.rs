use std::path::Path;

const RULE_FILES: &[&str] = &["AGENTS.md", "CLAUDE.md"];

/// 读取工作区项目规则（AGENTS.md / CLAUDE.md），作为系统指令的一部分。
pub fn load_project_rules(workspace: &Path) -> String {
    let mut rules = Vec::new();
    for name in RULE_FILES {
        let path = workspace.join(name);
        if let Ok(content) = std::fs::read_to_string(&path) {
            rules.push(format!("### {} 规则（必须遵守）\n{}", name, content.trim()));
        }
    }
    rules.join("\n\n")
}

pub fn build_system_prompt(configured: Option<&str>, rules: &str) -> String {
    let mut parts = Vec::new();
    if let Some(configured) = configured {
        parts.push(configured.to_string());
    }
    if !rules.is_empty() {
        parts.push(rules.to_string());
    }
    parts.push(
        "你是 OwO Agent SDK 驱动的智能体。工具调用必须经过权限审批；\
         被拒绝的操作不要重试同一参数，应寻找更安全的替代方案；\
         完成工作后给出简洁的最终汇报。"
            .to_string(),
    );
    parts.join("\n\n")
}
