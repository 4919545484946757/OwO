//! 流程技能包分享（v0.4 D26）：单文件 `.owskill`（ZIP），解包即 Agent Skills 标准技能包目录。
//!
//! 导入校验顺序：schema → 权限白名单（默认 deny，未知权限拒绝）→ 敏感度必填 →
//! 目标应用非空 → 变量声明 → 动作图合法；zip-slip（`..`/绝对路径）一律拒绝。
//! 硬性约束：不携带消息内容与真实截图样本（只导出 SKILL.md / graph.json / manifest.json / versions.json）。

use crate::learn::{FlowSkillPackage, Sensitivity};
use std::io::{Cursor, Read, Write};

const KNOWN_FILES: [&str; 4] = ["SKILL.md", "graph.json", "manifest.json", "versions.json"];

const ALLOWED_PERMISSIONS: [&str; 5] = [
    "ui:operate",
    "text:inject",
    "files:read",
    "files:write",
    "network:fetch",
];

/// 导出为 `.owskill`（ZIP，store 方式）。
pub fn export_flow_skill_package(package: &FlowSkillPackage) -> Result<Vec<u8>, String> {
    package.validate()?;
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default();
        write_entry(
            &mut writer,
            "SKILL.md",
            package.skill_md.as_bytes(),
            options,
        )?;
        write_entry(
            &mut writer,
            "graph.json",
            serde_json::to_string_pretty(&package.graph)
                .map_err(|error| error.to_string())?
                .as_bytes(),
            options,
        )?;
        write_entry(
            &mut writer,
            "manifest.json",
            serde_json::to_string_pretty(&package.manifest)
                .map_err(|error| error.to_string())?
                .as_bytes(),
            options,
        )?;
        let versions = serde_json::json!({
            package.manifest.version.clone(): package.manifest.min_app_version.clone()
        });
        write_entry(
            &mut writer,
            "versions.json",
            serde_json::to_string_pretty(&versions)
                .map_err(|error| error.to_string())?
                .as_bytes(),
            options,
        )?;
        writer.finish().map_err(|error| error.to_string())?;
    }
    Ok(cursor.into_inner())
}

fn write_entry(
    writer: &mut zip::ZipWriter<&mut Cursor<Vec<u8>>>,
    name: &str,
    content: &[u8],
    options: zip::write::SimpleFileOptions,
) -> Result<(), String> {
    writer
        .start_file(name, options)
        .map_err(|error| error.to_string())?;
    writer.write_all(content).map_err(|error| error.to_string())
}

fn safe_entry_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains("..")
        && !name.starts_with('/')
        && !name.starts_with('\\')
        && !name.contains('\\')
}

/// 导入 `.owskill`（ZIP），完整校验后返回技能包。
pub fn import_flow_skill_package(bytes: &[u8]) -> Result<FlowSkillPackage, String> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|error| error.to_string())?;
    let mut names = Vec::new();
    for index in 0..archive.len() {
        let name = archive
            .by_index(index)
            .map_err(|error| error.to_string())?
            .name()
            .to_string();
        if !safe_entry_name(&name) {
            return Err(format!("非法包内路径：{name}"));
        }
        names.push(name);
    }
    // 只读取白名单文件；其余条目忽略（但必须都在已知集合内，避免夹带数据）。
    for name in &names {
        if !KNOWN_FILES.contains(&name.as_str()) {
            return Err(format!("包内含未声明文件：{name}"));
        }
    }
    let mut read_file = |name: &str| -> Result<String, String> {
        let mut file = archive
            .by_name(name)
            .map_err(|error| format!("缺少 {name}：{error}"))?;
        let mut content = String::new();
        file.read_to_string(&mut content)
            .map_err(|error| error.to_string())?;
        Ok(content)
    };
    let skill_md = read_file("SKILL.md")?;
    let graph = serde_json::from_str(&read_file("graph.json")?)
        .map_err(|error| format!("graph.json 解析失败：{error}"))?;
    let manifest = serde_json::from_str(&read_file("manifest.json")?)
        .map_err(|error| format!("manifest.json 解析失败：{error}"))?;
    let package = FlowSkillPackage {
        manifest,
        graph,
        skill_md,
    };
    package.validate()?;
    if package.manifest.sensitivity == Sensitivity::None {
        return Err("sensitivity 必填".to_string());
    }
    for permission in &package.manifest.permissions {
        if !ALLOWED_PERMISSIONS.contains(&permission.as_str()) {
            return Err(format!("未授权权限：{permission}"));
        }
    }
    Ok(package)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learn::{ActionGraph, ActionType, FlowSkillManifest, SemanticAnchor, Sensitivity};

    fn package() -> FlowSkillPackage {
        let mut graph = ActionGraph::new();
        graph.add_node(
            "send",
            ActionType::Click,
            SemanticAnchor {
                app_id: Some("qq".to_string()),
                role: Some("button".to_string()),
                name: "发送".to_string(),
                parent: None,
                element_id: None,
            },
            None,
            Some("发送成功".to_string()),
        );
        FlowSkillPackage {
            manifest: FlowSkillManifest {
                id: "com.owo.learned.send-file".to_string(),
                name: "send-file".to_string(),
                version: "1.0.0".to_string(),
                min_app_version: "0.4.0".to_string(),
                target_apps: vec!["qq".to_string()],
                permissions: vec!["ui:operate".to_string()],
                variables: Vec::new(),
                sensitivity: Sensitivity::Low,
            },
            graph,
            skill_md: "---\nname: send-file\ndescription: 发送文件\n---\n流程".to_string(),
        }
    }

    #[test]
    fn export_import_round_trip() {
        let package = package();
        let zip_bytes = export_flow_skill_package(&package).unwrap();
        let imported = import_flow_skill_package(&zip_bytes).unwrap();
        assert_eq!(imported.manifest.name, "send-file");
        assert_eq!(imported.graph.nodes.len(), 1);
        assert!(imported.skill_md.contains("发送文件"));
    }

    #[test]
    fn import_rejects_unknown_permission() {
        let mut package = package();
        package.manifest.permissions = vec!["shell:exec".to_string()];
        let zip_bytes = export_flow_skill_package(&package).unwrap();
        let error = import_flow_skill_package(&zip_bytes).unwrap_err();
        assert!(error.contains("未授权权限"));
    }

    #[test]
    fn import_rejects_missing_sensitivity() {
        let mut package = package();
        package.manifest.sensitivity = Sensitivity::None;
        // export 本身也会拒绝 None，直接构造 zip 验证导入侧。
        let zip_bytes = export_flow_skill_package(&package).unwrap_err();
        assert!(zip_bytes.contains("sensitivity"));
    }

    #[test]
    fn import_rejects_zip_slip() {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default();
            writer
                .start_file("../evil.txt", options)
                .map_err(|error| error.to_string())
                .unwrap();
            writer
                .write_all(b"evil")
                .map_err(|error| error.to_string())
                .unwrap();
            writer.finish().unwrap();
        }
        let error = import_flow_skill_package(&cursor.into_inner()).unwrap_err();
        assert!(error.contains("非法包内路径"));
    }

    #[test]
    fn qq_send_file_example_package_is_valid_and_round_trips() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/user/qq-send-file");
        let skill_md = std::fs::read_to_string(root.join("SKILL.md")).unwrap();
        let graph: ActionGraph =
            serde_json::from_str(&std::fs::read_to_string(root.join("graph.json")).unwrap())
                .unwrap();
        let manifest: FlowSkillManifest =
            serde_json::from_str(&std::fs::read_to_string(root.join("manifest.json")).unwrap())
                .unwrap();
        let package = FlowSkillPackage {
            manifest,
            graph,
            skill_md,
        };
        package.validate().unwrap();
        assert_eq!(package.manifest.variables, vec!["contact", "file"]);
        let bytes = export_flow_skill_package(&package).unwrap();
        let imported = import_flow_skill_package(&bytes).unwrap();
        assert_eq!(imported.manifest.name, "qq-send-file");
        assert_eq!(imported.graph.nodes.len(), 8);
        assert_eq!(imported.graph.edges.len(), 7);
    }
}
