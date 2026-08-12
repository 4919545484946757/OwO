//! 技能包契约（v0.4 D18）：`SKILL.md + manifest.json + assets/ + tests/`。
//!
//! 与插件系统统一：技能 = 指令 + 资源 + 可选工具；权限在 manifest 声明，核心强制。
//! 内置技能包随包分发，安装到 `<data>/skills/<name>` 后与用户流程技能包同构。

use crate::skill::parse_frontmatter;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltinSkillManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub permissions: Vec<String>,
    #[serde(default)]
    pub min_app_version: String,
    #[serde(default)]
    pub tools: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SkillPackageInfo {
    pub name: String,
    pub path: PathBuf,
    pub description: String,
    pub permissions: Vec<String>,
    pub has_tests: bool,
}

/// 校验技能包目录结构（契约：SKILL.md frontmatter + manifest.json + tests/）。
pub fn validate_skill_package(dir: &Path) -> Result<SkillPackageInfo, String> {
    let skill_file = dir.join("SKILL.md");
    let content =
        std::fs::read_to_string(&skill_file).map_err(|error| format!("SKILL.md：{error}"))?;
    let (name, description, _) = parse_frontmatter(&content);
    let manifest_path = dir.join("manifest.json");
    let manifest: BuiltinSkillManifest = serde_json::from_str(
        &std::fs::read_to_string(&manifest_path)
            .map_err(|error| format!("manifest.json：{error}"))?,
    )
    .map_err(|error| format!("manifest.json 解析失败：{error}"))?;
    if manifest.id.is_empty() || manifest.name.is_empty() || manifest.version.is_empty() {
        return Err("manifest 缺少 id/name/version".to_string());
    }
    if manifest.permissions.is_empty() {
        return Err("manifest.permissions 不能为空（默认 deny）".to_string());
    }
    let has_tests = dir.join("tests").is_dir();
    if !has_tests {
        return Err("缺少 tests/ 契约测试目录".to_string());
    }
    Ok(SkillPackageInfo {
        name: name.unwrap_or_else(|| manifest.name.clone()),
        path: dir.to_path_buf(),
        description: if description.is_empty() {
            manifest.description.clone()
        } else {
            description
        },
        permissions: manifest.permissions,
        has_tests,
    })
}

/// 发现目录下合法的内置技能包。
pub fn discover_builtin_packages(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut packages = Vec::new();
    for entry in entries.flatten() {
        let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
        if is_dir
            && entry.path().join("SKILL.md").exists()
            && entry.path().join("manifest.json").exists()
            && validate_skill_package(&entry.path()).is_ok()
        {
            packages.push(entry.path());
        }
    }
    packages.sort();
    packages
}

/// 把内置技能包安装到 `<data_root>/skills/`（已存在则跳过）。
pub fn install_builtin_packages(builtin_root: &Path, data_root: &Path) -> Result<usize, String> {
    let mut installed = 0usize;
    for package in discover_builtin_packages(builtin_root) {
        let info = validate_skill_package(&package)?;
        let target = data_root.join("skills").join(&info.name);
        if target.exists() {
            continue;
        }
        copy_dir(&package, &target)?;
        installed += 1;
    }
    Ok(installed)
}

fn copy_dir(source: &Path, target: &Path) -> Result<(), String> {
    std::fs::create_dir_all(target).map_err(|error| error.to_string())?;
    for entry in std::fs::read_dir(source).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir(&from, &to)?;
        } else if file_type.is_file() {
            std::fs::copy(&from, &to).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_package(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join("tests")).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: 测试技能\n---\n执行步骤。"),
        )
        .unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_string_pretty(&BuiltinSkillManifest {
                id: format!("com.example.{name}"),
                name: name.to_string(),
                version: "1.0.0".to_string(),
                description: "测试技能".to_string(),
                permissions: vec!["files:read".to_string()],
                min_app_version: "0.4.0".to_string(),
                tools: Vec::new(),
            })
            .unwrap(),
        )
        .unwrap();
        std::fs::write(dir.join("tests").join("case-1.md"), "# 用例 1").unwrap();
        dir
    }

    #[test]
    fn validates_package_contract() {
        let root = std::env::temp_dir().join(format!("owo-pack-{}", uuid::Uuid::new_v4()));
        let dir = write_package(&root, "documents");
        let info = validate_skill_package(&dir).unwrap();
        assert_eq!(info.name, "documents");
        assert!(info.has_tests);
        assert_eq!(info.permissions, vec!["files:read"]);

        std::fs::remove_file(dir.join("manifest.json")).unwrap();
        assert!(validate_skill_package(&dir).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discovers_and_installs_builtin_packages() {
        let root = std::env::temp_dir().join(format!("owo-pack-root-{}", uuid::Uuid::new_v4()));
        write_package(&root, "documents");
        write_package(&root, "pdf");
        assert_eq!(discover_builtin_packages(&root).len(), 2);

        let data = root.join("data");
        let count = install_builtin_packages(&root, &data).unwrap();
        assert_eq!(count, 2);
        assert!(data.join("skills/documents/SKILL.md").exists());
        assert!(data.join("skills/pdf/manifest.json").exists());
        assert_eq!(install_builtin_packages(&root, &data).unwrap(), 0); // 已存在跳过
        let _ = std::fs::remove_dir_all(&root);
    }
}
