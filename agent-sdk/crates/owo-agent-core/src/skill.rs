//! Agent Skills：遵循 Agent Skills 开放标准（目录 + SKILL.md），
//! 支持 frontmatter（name/description）与正文指令。

use std::path::{Path, PathBuf};

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
}

impl SkillRegistry {
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
}
