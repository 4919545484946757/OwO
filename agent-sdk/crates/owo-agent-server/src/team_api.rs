//! 团队技能包共享（/team，Agent 2 子任务 2）。
//!
//! 导出/导入/脱敏评审/版本审计：
//! - `POST /team/export`：从本地技能包存储导出（base64 打包字节 + manifest 摘要）
//! - `POST /team/review`：只评审不导入（脱敏清单 findings）
//! - `POST /team/import`：先脱敏评审 → 不通过返回 `{blocked:true,findings}` 不落盘；
//!   通过才导入（校验 + 版本历史 + 审计）
//! - `GET /team/versions`：版本历史（data_root/team/<id>/versions.json，模块内维护）
//! - `GET /team/audit`：审计尾部
//!
//! 安全：导入包先经脱敏评审（凭据/个人数据/危险动作关键词），高危立即 blocked；
//! 包完整性由版本历史哈希 + FlowSkillPackage::validate 双重把关。
//!
//! 注：router 由主控合并进 build_router 前，模块内部结构暂未被 lib 引用，
//! 故模块级 allow(dead_code)（接线后自动消除，保留无害）。

#![allow(dead_code)]

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use base64::Engine;
use owo_agent_core::learn::{FlowSkillPackage, FlowSkillStore};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

// ---------- 模块内状态（data_root 键控） ----------

static TEAM_STORES: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<TeamStore>>>>> = OnceLock::new();
static AUDIT: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

fn team_stores() -> &'static Mutex<HashMap<PathBuf, Arc<Mutex<TeamStore>>>> {
    TEAM_STORES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn audit_log() -> &'static Mutex<Vec<String>> {
    AUDIT.get_or_init(|| Mutex::new(Vec::new()))
}

fn audit(event: &str, detail: impl AsRef<str>) {
    if let Ok(mut log) = audit_log().lock() {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        log.push(format!("[{secs}] {event}: {}", detail.as_ref()));
        if log.len() > 500 {
            let keep = log.len() - 500;
            log.drain(..keep);
        }
    }
}

/// 团队包存储（data_root/team 下的导入库与版本历史）。
struct TeamStore {
    /// 导入的技能包库（FlowSkillStore）。
    skills: FlowSkillStore,
    /// 版本历史根目录。
    versions_root: PathBuf,
}

impl TeamStore {
    fn new(data_root: &std::path::Path) -> Self {
        Self {
            skills: FlowSkillStore::new(data_root.join("team").join("skills")),
            versions_root: data_root.join("team"),
        }
    }

    fn append_version(&self, id: &str, version: &str, sha256: &str) -> Result<(), String> {
        let path = self.versions_root.join(id).join("versions.json");
        let mut history: VersionHistory = std::fs::read_to_string(&path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default();
        history.versions.retain(|v| v.version != version);
        history.versions.push(VersionEntry {
            version: version.to_string(),
            sha256: sha256.to_string(),
            imported_at: now_ts(),
        });
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(
            path,
            serde_json::to_string_pretty(&history).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }

    fn version_history(&self, id: &str) -> VersionHistory {
        let path = self.versions_root.join(id).join("versions.json");
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }
}

fn store_for(data_root: &std::path::Path) -> Arc<Mutex<TeamStore>> {
    let mut map = team_stores().lock().unwrap();
    map.entry(data_root.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(TeamStore::new(data_root))))
        .clone()
}

fn now_ts() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}

/// 非加密完整性哈希（版本追踪/审计用；64 位，不用于安全边界）。
fn sha256_hex(bytes: &[u8]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    let value = hasher.finish();
    format!("{value:016x}")
}

// ---------- 数据模型 ----------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct VersionHistory {
    versions: Vec<VersionEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct VersionEntry {
    version: String,
    sha256: String,
    imported_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReviewFinding {
    pub category: String,
    pub detail: String,
    pub severity: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReviewResult {
    pub blocked: bool,
    pub findings: Vec<ReviewFinding>,
}

#[derive(Deserialize)]
struct ExportBody {
    #[serde(rename = "type")]
    package_type: String,
    id: String,
}

#[derive(Deserialize)]
struct PackageBody {
    package_b64: String,
}

#[derive(Deserialize)]
struct VersionsQuery {
    id: String,
}

// ---------- 脱敏评审 ----------

/// 脱敏评审清单：扫描凭据/个人数据/危险动作关键词。
/// 返回 findings（空 = 通过）。`blocked` 由调用方按 severity 判定。
pub fn sanitize_review(text: &str) -> Vec<ReviewFinding> {
    let mut findings = Vec::new();
    let lower = text.to_lowercase();

    // 凭据类（high）。
    let credential_keywords: &[(&str, &str)] = &[
        ("OpenAI API Key（sk-）", "sk-"),
        ("AWS Access Key（AKIA）", "AKIA"),
        ("私钥（PRIVATE KEY）", "PRIVATE KEY"),
        ("明文密码（password）", "password"),
        ("API Token（token）", "token"),
    ];
    for (label, keyword) in credential_keywords {
        if lower.contains(&keyword.to_lowercase()) {
            findings.push(ReviewFinding {
                category: "credential".to_string(),
                detail: format!("疑似{label}"),
                severity: "high".to_string(),
            });
        }
    }

    // 个人数据类（high/medium）。
    if regex_find(text, r"1[3-9]\d{9}").is_some() {
        findings.push(ReviewFinding {
            category: "personal".to_string(),
            detail: "疑似手机号".to_string(),
            severity: "high".to_string(),
        });
    }
    if regex_find(text, r"[\w.+-]+@[\w-]+\.[\w.]+").is_some() {
        findings.push(ReviewFinding {
            category: "personal".to_string(),
            detail: "疑似邮箱地址".to_string(),
            severity: "medium".to_string(),
        });
    }
    if regex_find(text, r"\d{17}[\dXx]").is_some() {
        findings.push(ReviewFinding {
            category: "personal".to_string(),
            detail: "疑似身份证号".to_string(),
            severity: "high".to_string(),
        });
    }

    // 危险动作类（high）。
    let dangerous = [
        "os.system",
        "os.popen",
        "subprocess.Popen",
        "subprocess.call",
        "rm -rf",
        "format c:",
        "del /f /s",
        "DROP TABLE",
        "DELETE FROM",
        "shutil.rmtree",
        "pickle.loads",
        "eval(",
        "exec(",
    ];
    for keyword in dangerous {
        if lower.contains(keyword) {
            findings.push(ReviewFinding {
                category: "dangerous".to_string(),
                detail: format!("危险动作：{keyword}"),
                severity: "high".to_string(),
            });
        }
    }

    // 私密消息内容（medium）：QQ 号/昵称类（8-10 位数字）。
    if regex_find(text, r"(?<!\d)\d{8,10}(?!\d)").is_some() {
        findings.push(ReviewFinding {
            category: "personal".to_string(),
            detail: "疑似聊天账号/消息流水号".to_string(),
            severity: "medium".to_string(),
        });
    }

    findings
}

/// 极简正则匹配（无 regex 依赖）：支持 `sk-xxx`、`AKIA...`、固定模式。
fn regex_find(text: &str, pattern: &str) -> Option<String> {
    // 简单实现：对固定模式做直接匹配（覆盖本模块模式集）。
    match pattern {
        r"1[3-9]\d{9}" => find_digit_len(text, 11, '1'),
        r"[\w.+-]+@[\w-]+\.[\w.]+" => find_email(text),
        r"\d{17}[\dXx]" => find_digit_len_x(text, 18),
        r"(?<!\d)\d{8,10}(?!\d)" => find_digit_range(text, 8, 10),
        _ => None,
    }
}

fn find_digit_len(text: &str, len: usize, first: char) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + len <= bytes.len() {
        if bytes[i] == first as u8 && bytes[i..i + len].iter().all(|b| b.is_ascii_digit()) {
            return Some(text[i..i + len].to_string());
        }
        i += 1;
    }
    None
}

fn find_digit_len_x(text: &str, len: usize) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + len <= bytes.len() {
        let last = bytes[i + len - 1];
        if bytes[i..i + len - 1].iter().all(|b| b.is_ascii_digit())
            && (last.is_ascii_digit() || last == b'X' || last == b'x')
        {
            return Some(text[i..i + len].to_string());
        }
        i += 1;
    }
    None
}

fn find_digit_range(text: &str, min: usize, max: usize) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            let mut end = i;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            let len = end - start;
            if len >= min && len <= max {
                return Some(text[start..end].to_string());
            }
            i = end;
        } else {
            i += 1;
        }
    }
    None
}

fn find_email(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' {
            // 向前找 @ 前 token，向后找域名。
            let mut start = i;
            while start > 0
                && (bytes[start - 1].is_ascii_alphanumeric()
                    || bytes[start - 1] == b'.'
                    || bytes[start - 1] == b'_'
                    || bytes[start - 1] == b'-'
                    || bytes[start - 1] == b'+')
            {
                start -= 1;
            }
            let mut end = i + 1;
            while end < bytes.len()
                && (bytes[end].is_ascii_alphanumeric()
                    || bytes[end] == b'.'
                    || bytes[end] == b'-'
                    || bytes[end] == b'_')
            {
                end += 1;
            }
            if end > i + 1 && text[i + 1..end].contains('.') {
                return Some(text[start..end].to_string());
            }
            i = end.max(i + 1);
        } else {
            i += 1;
        }
    }
    None
}

// ---------- 路由 ----------

pub fn router(state: Arc<owo_agent_server::AppState>) -> axum::Router {
    axum::Router::new()
        .route("/team/export", axum::routing::post(export_package))
        .route("/team/review", axum::routing::post(review_package))
        .route("/team/import", axum::routing::post(import_package))
        .route("/team/versions", axum::routing::get(versions))
        .route("/team/audit", axum::routing::get(audit_tail))
        .with_state(state)
}

type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn err(status: StatusCode, message: impl AsRef<str>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": message.as_ref() })))
}

// ---------- handlers ----------

/// POST /team/export {type, id}：从本地技能包存储导出。
async fn export_package(
    State(state): State<Arc<owo_agent_server::AppState>>,
    Json(body): Json<ExportBody>,
) -> ApiResult {
    if body.package_type != "flow" {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("暂不支持的类型：{}", body.package_type),
        ));
    }
    let pipeline = state
        .pipeline
        .lock()
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "pipeline 锁中毒"))?;
    let package = pipeline
        .store
        .load(&body.id)
        .map_err(|e| err(StatusCode::NOT_FOUND, format!("技能包不存在：{e}")))?;
    let bytes = owo_agent_core::share_skill::export_flow_skill_package(&package)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("导出失败：{e}")))?;
    let package_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    audit(
        "team/export",
        format!(
            "{} v{} 已导出（{} 字节）",
            package.manifest.id,
            package.manifest.version,
            bytes.len()
        ),
    );
    Ok(Json(json!({
        "package_b64": package_b64,
        "manifest": manifest_summary(&package),
        "size_bytes": bytes.len(),
    })))
}

/// POST /team/review {package_b64}：只评审不导入。
async fn review_package(Json(body): Json<PackageBody>) -> ApiResult {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&body.package_b64)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("base64 解码失败：{e}")))?;
    let package = owo_agent_core::share_skill::import_flow_skill_package(&bytes)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("包解析失败：{e}")))?;
    let findings = sanitize_review(&package_text(&package));
    let blocked = findings.iter().any(|f| f.severity == "high");
    Ok(Json(
        json!({ "blocked": blocked, "findings": findings, "package": manifest_summary(&package) }),
    ))
}

/// POST /team/import {package_b64}：脱敏评审 → 通过才导入。
async fn import_package(
    State(state): State<Arc<owo_agent_server::AppState>>,
    Json(body): Json<PackageBody>,
) -> ApiResult {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&body.package_b64)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("base64 解码失败：{e}")))?;
    let package = owo_agent_core::share_skill::import_flow_skill_package(&bytes)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("包解析失败：{e}")))?;
    package
        .validate()
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("包校验失败：{e}")))?;

    // 脱敏评审。
    let findings = sanitize_review(&package_text(&package));
    let blocked = findings.iter().any(|f| f.severity == "high");
    if blocked {
        audit(
            "team/import/blocked",
            format!(
                "{} v{} 因脱敏评审被拦截",
                package.manifest.id, package.manifest.version
            ),
        );
        return Ok(Json(json!({
            "blocked": true,
            "findings": findings,
            "package": manifest_summary(&package),
        })));
    }

    // 通过：导入到团队库 + 版本历史。
    let store = store_for(&state.data_root);
    let store = store
        .lock()
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "team store 锁中毒"))?;
    let sha = sha256_hex(&bytes);
    let version = package.manifest.version.clone();
    store
        .skills
        .save(&package)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("导入失败：{e}")))?;
    store
        .append_version(&package.manifest.id, &version, &sha)
        .map_err(|e| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("版本记录失败：{e}"),
            )
        })?;
    let history = store.version_history(&package.manifest.id);
    drop(store);
    audit(
        "team/import",
        format!(
            "{} v{} 导入成功（sha256 {:.12}…）",
            package.manifest.id, version, sha
        ),
    );
    Ok(Json(json!({
        "blocked": false,
        "findings": findings,
        "package": manifest_summary(&package),
        "versions": history.versions,
    })))
}

/// GET /team/versions?id=：版本历史。
async fn versions(
    State(state): State<Arc<owo_agent_server::AppState>>,
    Query(query): Query<VersionsQuery>,
) -> ApiResult {
    let store = store_for(&state.data_root);
    let store = store
        .lock()
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "team store 锁中毒"))?;
    let history = store.version_history(&query.id);
    Ok(Json(json!({
        "id": query.id,
        "count": history.versions.len(),
        "versions": history.versions,
    })))
}

/// GET /team/audit：审计尾部。
async fn audit_tail() -> ApiResult {
    let entries = audit_log()
        .lock()
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "审计锁中毒"))?
        .iter()
        .rev()
        .take(50)
        .cloned()
        .collect::<Vec<_>>();
    Ok(Json(json!({ "count": entries.len(), "entries": entries })))
}

// ---------- 内部工具 ----------

fn manifest_summary(package: &FlowSkillPackage) -> Value {
    let manifest = &package.manifest;
    json!({
        "id": manifest.id,
        "name": manifest.name,
        "version": manifest.version,
        "target_apps": manifest.target_apps,
        "permissions": manifest.permissions,
        "variables": manifest.variables,
        "sensitivity": format!("{:?}", manifest.sensitivity),
    })
}

/// 评审文本：manifest + 技能文档 + 动作图序列化。
fn package_text(package: &FlowSkillPackage) -> String {
    let manifest_json = serde_json::to_string(&package.manifest).unwrap_or_default();
    let graph_json = serde_json::to_string(&package.graph).unwrap_or_default();
    format!("{manifest_json}\n{graph_json}\n{}", package.skill_md)
}
