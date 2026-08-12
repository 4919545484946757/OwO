//! Agent Skills：遵循 Agent Skills 开放标准（目录 + SKILL.md），
//! 支持 frontmatter（name/description）与正文指令。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub instructions: String,
}

impl Skill {
    pub fn load(skill_file: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(skill_file).map_err(|error| error.to_string())?;
        let (name, description, instructions) = parse_frontmatter(&content);
        let name = name.unwrap_or_else(|| {
            skill_file
                .parent()
                .and_then(|parent| parent.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unnamed".to_string())
        });
        Ok(Self {
            name,
            description,
            path: skill_file.to_path_buf(),
            instructions,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct SkillRegistry {
    skills: Vec<Skill>,
    /// 运行时禁用集合（进程内共享，设置页切换即时生效；重启时从 settings.json 重建）。
    disabled: Arc<Mutex<HashSet<String>>>,
}

impl SkillRegistry {
    /// 注入运行时禁用集合（与设置持久化共用同一集合）。
    pub fn set_disabled(&mut self, disabled: Arc<Mutex<HashSet<String>>>) {
        self.disabled = disabled;
    }

    pub fn disabled_set(&self) -> Arc<Mutex<HashSet<String>>> {
        Arc::clone(&self.disabled)
    }

    pub fn is_enabled(&self, name: &str) -> bool {
        !self
            .disabled
            .lock()
            .map(|disabled| disabled.contains(name))
            .unwrap_or(false)
    }

    /// 从全局数据目录 `<data>/skills` 与工作区 `.agents/skills` 发现技能。
    pub fn discover(workspace: &Path, data_root: &Path) -> Self {
        let mut registry = Self::default();
        let mut roots = vec![
            data_root.join("skills"),
            workspace.join(".agents").join("skills"),
        ];
        for root in roots.drain(..) {
            let Ok(entries) = std::fs::read_dir(&root) else {
                continue;
            };
            for entry in entries.flatten() {
                let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
                if !is_dir {
                    continue;
                }
                let skill_file = entry.path().join("SKILL.md");
                if !skill_file.exists() {
                    continue;
                }
                if let Ok(skill) = Skill::load(&skill_file) {
                    if !registry
                        .skills
                        .iter()
                        .any(|existing| existing.name == skill.name)
                    {
                        registry.skills.push(skill);
                    }
                }
            }
        }
        registry
            .skills
            .sort_by(|left, right| left.name.cmp(&right.name));
        registry
    }

    pub fn list(&self) -> Vec<&Skill> {
        self.skills.iter().collect()
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|skill| skill.name == name)
    }

    /// 仅返回启用的技能（供系统提示注入与 use_skill 使用）。
    pub fn list_enabled(&self) -> Vec<&Skill> {
        self.skills
            .iter()
            .filter(|skill| self.is_enabled(&skill.name))
            .collect()
    }

    pub fn get_enabled(&self, name: &str) -> Option<&Skill> {
        self.skills
            .iter()
            .find(|skill| skill.name == name && self.is_enabled(&skill.name))
    }
}

pub(crate) fn parse_frontmatter(content: &str) -> (Option<String>, String, String) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (None, String::new(), content.to_string());
    }
    let rest = &trimmed[3..];
    let Some(end) = rest.find("\n---") else {
        return (None, String::new(), content.to_string());
    };
    let frontmatter = &rest[..end];
    let body = rest[end + 4..].trim_start();
    let mut name = None;
    let mut description = String::new();
    for line in frontmatter.lines() {
        if let Some(value) = line.strip_prefix("name:") {
            name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("description:") {
            description = value.trim().to_string();
        }
    }
    (name, description, body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter_and_body() {
        let content = "---\nname: demo\ndescription: 测试技能\n---\n执行 A 步骤。";
        let (name, description, instructions) = parse_frontmatter(content);
        assert_eq!(name.as_deref(), Some("demo"));
        assert_eq!(description, "测试技能");
        assert!(instructions.contains("执行 A 步骤"));
    }

    #[test]
    fn discovers_skills_from_workspace_and_data() {
        let workspace =
            std::env::temp_dir().join(format!("owo-skill-workspace-{}", uuid::Uuid::new_v4()));
        let data = std::env::temp_dir().join(format!("owo-skill-data-{}", uuid::Uuid::new_v4()));
        let skill_dir = workspace.join(".agents").join("skills").join("demo");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: demo\ndescription: 测试技能\n---\n执行 A。",
        )
        .unwrap();
        let global_dir = data.join("skills").join("global-skill");
        std::fs::create_dir_all(&global_dir).unwrap();
        std::fs::write(global_dir.join("SKILL.md"), "全局技能正文").unwrap();

        let registry = SkillRegistry::discover(&workspace, &data);
        let names: Vec<&str> = registry
            .list()
            .iter()
            .map(|skill| skill.name.as_str())
            .collect();
        assert!(names.contains(&"demo"));
        assert!(names.contains(&"global-skill"));
        assert!(registry
            .get("demo")
            .unwrap()
            .instructions
            .contains("执行 A"));
        let _ = std::fs::remove_dir_all(&workspace);
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn disabled_skills_are_filtered_and_shared() {
        let workspace =
            std::env::temp_dir().join(format!("owo-skill-disable-ws-{}", uuid::Uuid::new_v4()));
        let data =
            std::env::temp_dir().join(format!("owo-skill-disable-data-{}", uuid::Uuid::new_v4()));
        for name in ["demo", "other"] {
            let dir = workspace.join(".agents").join("skills").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: 测试\n---\n正文 {name}。"),
            )
            .unwrap();
        }
        let mut registry = SkillRegistry::discover(&workspace, &data);
        let disabled = Arc::new(Mutex::new(HashSet::from(["demo".to_string()])));
        registry.set_disabled(Arc::clone(&disabled));
        assert!(!registry.is_enabled("demo"));
        assert!(registry.is_enabled("other"));
        let names: Vec<&str> = registry
            .list_enabled()
            .iter()
            .map(|skill| skill.name.as_str())
            .collect();
        assert!(!names.contains(&"demo"));
        assert!(names.contains(&"other"));
        assert!(registry.get_enabled("demo").is_none());
        assert!(registry.get_enabled("other").is_some());
        // 共享集合运行时变更即时生效。
        disabled.lock().unwrap().remove("demo");
        assert!(registry.is_enabled("demo"));
        assert!(registry.get_enabled("demo").is_some());
        let _ = std::fs::remove_dir_all(&workspace);
        let _ = std::fs::remove_dir_all(&data);
    }
}
