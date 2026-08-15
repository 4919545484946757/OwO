//! 插件市场 HTTP API（M4b / Lane B）。
//!
//! 前缀 `/plugins/market`：目录（本地清单 + market.json 合并含 has_update）、
//! seed 示例条目、versions 兼容解析、verify/install/update/uninstall、
//! 风险扫描、审计尾部。
//!
//! 安全默认 deny：安装前先静态扫描（高危拒绝）；签名默认开启
//! （`OWO_PLUGIN_REQUIRE_SIGNATURE=0` 关闭，供联调/测试）；更新失败回滚由
//! `PluginManager` 保证；所有写操作留审计（模块内日志 + 报告审计字段）。

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use owo_agent_core::plugin::{
    discover_plugins, scan_plugin_for_risks, MarketUpdateManifest, PluginManager, PluginManifest,
    VersionsJson,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

// ---------- 模块内状态（data_root 键控，避免跨测试污染） ----------

static MANAGERS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<PluginManager>>>>> = OnceLock::new();
static AUDIT: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

fn managers() -> &'static Mutex<HashMap<PathBuf, Arc<Mutex<PluginManager>>>> {
    MANAGERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn audit_log() -> &'static Mutex<Vec<String>> {
    AUDIT.get_or_init(|| Mutex::new(Vec::new()))
}

/// 按 data_root 键控取（或创建）PluginManager；每次调用同步 require_signature
/// 环境变量（静态缓存不会携带跨测试/跨进程 env 变化）。
fn manager_for(data_root: &Path) -> Arc<Mutex<PluginManager>> {
    let mut map = managers().lock().unwrap();
    let manager = map
        .entry(data_root.to_path_buf())
        .or_insert_with(|| {
            Arc::new(Mutex::new(PluginManager::new(
                data_root.to_path_buf(),
                app_version(),
            )))
        })
        .clone();
    if let Ok(mut mgr) = manager.lock() {
        mgr.set_require_signature(require_signature());
    }
    manager
}

/// 默认强制签名；`OWO_PLUGIN_REQUIRE_SIGNATURE=0` 关闭。
fn require_signature() -> bool {
    std::env::var("OWO_PLUGIN_REQUIRE_SIGNATURE")
        .map(|v| v.trim() != "0")
        .unwrap_or(true)
}

fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn now_ts() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}

fn audit(event: &str, detail: impl AsRef<str>) {
    if let Ok(mut log) = audit_log().lock() {
        log.push(format!("[{}] {event}: {}", now_ts(), detail.as_ref()));
        if log.len() > 500 {
            let keep = log.len() - 500;
            log.drain(..keep);
        }
    }
}

// ---------- 目录 / 来源解析 ----------

/// 解析插件目录参数：绝对路径直接用；相对路径按 workspace 解析。
fn resolve_dir(workspace: &Path, raw: &str) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    }
}

fn read_market(data_root: &Path) -> MarketUpdateManifest {
    let path = data_root.join("plugins").join("market.json");
    MarketUpdateManifest::load(&path).unwrap_or_default()
}

fn write_market(data_root: &Path, manifest: &MarketUpdateManifest) -> Result<(), String> {
    let path = data_root.join("plugins").join("market.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

// ---------- 请求/响应结构 ----------

#[derive(Deserialize)]
struct DirBody {
    dir: String,
}

#[derive(Deserialize)]
struct UpdateBody {
    id: String,
    dir: String,
}

#[derive(Deserialize)]
struct UninstallBody {
    id: String,
}

#[derive(Deserialize)]
struct SeedBody {
    entries: Vec<SeedEntry>,
}

#[derive(Deserialize)]
#[allow(dead_code)] // 协议保留字段（seed 表单/未来服务端使用）
struct SeedEntry {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    min_app_version: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    url: String,
}

#[derive(Deserialize)]
struct VersionsQuery {
    id: String,
    #[serde(default)]
    app: Option<String>,
}

#[derive(Deserialize)]
struct ScanQuery {
    dir: String,
}

#[derive(Deserialize)]
struct AuditQuery {
    #[serde(default)]
    n: Option<usize>,
}

// ---------- 路由 ----------

pub fn router(state: Arc<owo_agent_server::AppState>) -> axum::Router {
    axum::Router::new()
        .route("/plugins/market", axum::routing::get(catalog))
        .route("/plugins/market/seed", axum::routing::post(seed))
        .route("/plugins/market/versions", axum::routing::get(versions))
        .route("/plugins/market/verify", axum::routing::post(verify))
        .route("/plugins/market/install", axum::routing::post(install))
        .route("/plugins/market/update", axum::routing::post(update))
        .route("/plugins/market/uninstall", axum::routing::post(uninstall))
        .route("/plugins/market/scan", axum::routing::get(scan))
        .route("/plugins/market/audit", axum::routing::get(audit_tail))
        .with_state(state)
}

type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn err(status: StatusCode, message: impl AsRef<str>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": message.as_ref() })))
}

fn ok(value: Value) -> ApiResult {
    Ok(Json(value))
}

// ---------- handlers ----------

/// GET /plugins/market：目录（本地清单 + market.json 合并，含 has_update）。
async fn catalog(State(state): State<Arc<owo_agent_server::AppState>>) -> ApiResult {
    let app = app_version();
    let market = read_market(&state.data_root);
    let local = discover_plugins(&state.workspace, &state.data_root);

    let mut plugins: Vec<Value> = Vec::new();
    for (path, manifest) in &local {
        let has_update = market.has_update(&manifest.id, &manifest.version, &app);
        let risks = scan_summary(path, manifest);
        plugins.push(json!({
            "id": manifest.id,
            "name": manifest.name,
            "version": manifest.version,
            "description": manifest.description,
            "source": "local",
            "path": path.parent().map(|p| p.display().to_string()).unwrap_or_default(),
            "enabled": true,
            "has_update": has_update,
            "risks": risks,
        }));
    }
    // market 中独有条目（尚未本地安装）。
    for entry in &market.plugins {
        if !local.iter().any(|(_, m)| m.id == entry.id) {
            plugins.push(json!({
                "id": entry.id,
                "name": entry.id,
                "version": entry.latest_version,
                "description": entry.min_app_version.as_deref().unwrap_or(""),
                "source": "market",
                "has_update": false,
                "risks": [],
            }));
        }
    }
    plugins.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    ok(json!({
        "app_version": app,
        "require_signature": require_signature(),
        "plugins": plugins,
    }))
}

/// POST /plugins/market/seed：写入示例市场条目（market.json）。
async fn seed(
    State(state): State<Arc<owo_agent_server::AppState>>,
    Json(body): Json<SeedBody>,
) -> ApiResult {
    let mut market = read_market(&state.data_root);
    for entry in body.entries {
        if entry.id.trim().is_empty() {
            return Err(err(StatusCode::BAD_REQUEST, "entry.id 不能为空"));
        }
        // 同 id 覆盖（保留已有签名/最低版本语义：合并 min_app_version）。
        if let Some(existing) = market.plugins.iter_mut().find(|e| e.id == entry.id) {
            existing.latest_version = entry.version;
            if entry.min_app_version.is_some() {
                existing.min_app_version = entry.min_app_version.clone();
            }
        } else {
            market
                .plugins
                .push(owo_agent_core::plugin::MarketPluginEntry {
                    id: entry.id,
                    latest_version: entry.version,
                    min_app_version: entry.min_app_version,
                    signature: None,
                });
        }
    }
    write_market(&state.data_root, &market)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let count = market.plugins.len();
    audit("market/seed", format!("写入 {} 条示例条目", count));
    ok(json!({ "ok": true, "entries": count }))
}

/// GET /plugins/market/versions?plugin={id}&app={version}：兼容版本解析。
async fn versions(
    State(state): State<Arc<owo_agent_server::AppState>>,
    Query(query): Query<VersionsQuery>,
) -> ApiResult {
    let app = query.app.unwrap_or_else(app_version);
    let mut found = None;
    for root in [
        state.workspace.join("plugins"),
        state.data_root.join("plugins"),
    ] {
        let path = root.join(&query.id).join("versions.json");
        if let Ok(versions) = VersionsJson::load(&path) {
            found = Some(versions);
            break;
        }
    }
    let versions = found.ok_or_else(|| {
        err(
            StatusCode::NOT_FOUND,
            format!("插件 {} 无 versions.json", query.id),
        )
    })?;
    let latest = versions.resolve_compatible(&app);
    ok(json!({
        "plugin": query.id,
        "app_version": app,
        "compatibility": versions.compatibility,
        "latest_compatible": latest,
    }))
}

/// POST /plugins/market/verify {dir}：校验插件目录（签名/扫描/版本）。
async fn verify(
    State(state): State<Arc<owo_agent_server::AppState>>,
    Json(body): Json<DirBody>,
) -> ApiResult {
    let dir = resolve_dir(&state.workspace, &body.dir);
    let manager = manager_for(&state.data_root);
    let manager = manager
        .lock()
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "manager 锁中毒"))?;
    let report = manager
        .verify_plugin_dir(&dir)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("校验失败：{e}")))?;
    drop(manager);
    audit(
        "market/verify",
        format!("{} v{} 校验通过", report.id, report.version),
    );
    ok(json!({ "report": report }))
}

/// POST /plugins/market/install {dir}：安全前置扫描 + 安装。
async fn install(
    State(state): State<Arc<owo_agent_server::AppState>>,
    Json(body): Json<DirBody>,
) -> ApiResult {
    let dir = resolve_dir(&state.workspace, &body.dir);
    // 安全前置：静态扫描，高危拒绝（签名由 verify_plugin_dir 把关）。
    pre_scan(&dir).map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    let manager = manager_for(&state.data_root);
    let manager = manager
        .lock()
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "manager 锁中毒"))?;
    let report = manager
        .install(&dir)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("安装失败：{e}")))?;
    drop(manager);
    audit(
        "market/install",
        format!("{} v{} → {:?}", report.id, report.version, report.state),
    );
    ok(json!({ "report": report }))
}

/// POST /plugins/market/update {id, dir}：更新（先备份，失败回滚）。
async fn update(
    State(state): State<Arc<owo_agent_server::AppState>>,
    Json(body): Json<UpdateBody>,
) -> ApiResult {
    let dir = resolve_dir(&state.workspace, &body.dir);
    pre_scan(&dir).map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    let backup_root = state.data_root.join("plugins").join("backups");
    let manager = manager_for(&state.data_root);
    let manager = manager
        .lock()
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "manager 锁中毒"))?;
    let report = manager
        .update(&dir, &backup_root)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("更新失败：{e}")))?;
    drop(manager);
    audit(
        "market/update",
        format!(
            "{} → v{}（回滚状态 {:?}）",
            body.id, report.version, report.state
        ),
    );
    ok(json!({ "report": report }))
}

/// POST /plugins/market/uninstall {id}：卸载并返回被移除文件。
async fn uninstall(
    State(state): State<Arc<owo_agent_server::AppState>>,
    Json(body): Json<UninstallBody>,
) -> ApiResult {
    let target = state.data_root.join("plugins").join(&body.id);
    let removed = list_files(&target);
    let manager = manager_for(&state.data_root);
    let manager = manager
        .lock()
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "manager 锁中毒"))?;
    let report = manager
        .uninstall(&body.id)
        .map_err(|e| err(StatusCode::NOT_FOUND, format!("卸载失败：{e}")))?;
    drop(manager);
    audit(
        "market/uninstall",
        format!("{} 移除 {} 个文件", body.id, removed.len()),
    );
    ok(json!({ "ok": true, "removed": removed, "audit": report }))
}

/// GET /plugins/market/scan?dir=：风险扫描摘要（不安装）。
async fn scan(
    State(state): State<Arc<owo_agent_server::AppState>>,
    Query(query): Query<ScanQuery>,
) -> ApiResult {
    let dir = resolve_dir(&state.workspace, &query.dir);
    let risks = scan_dir_risks(&dir)?;
    ok(json!({
        "dir": dir.display().to_string(),
        "pass": risks.is_empty(),
        "risks": risks,
    }))
}

/// GET /plugins/market/audit?n=：审计尾部。
async fn audit_tail(Query(query): Query<AuditQuery>) -> ApiResult {
    let n = query.n.unwrap_or(20).min(200);
    let entries = audit_log()
        .lock()
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "审计锁中毒"))?
        .iter()
        .rev()
        .take(n)
        .cloned()
        .collect::<Vec<_>>();
    ok(json!({ "count": entries.len(), "entries": entries }))
}

// ---------- 内部工具 ----------

/// 安装前静态扫描（高危拒绝）。签名校验由 verify_plugin_dir 完成。
fn pre_scan(dir: &Path) -> Result<(), String> {
    let risks = match scan_dir_risks(dir) {
        Ok(risks) => risks,
        Err((_, body)) => {
            return Err(body
                .0
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("目录无效")
                .to_string())
        }
    };
    if risks.is_empty() {
        Ok(())
    } else {
        Err(format!("静态扫描未通过：{}", risks.join("；")))
    }
}

fn scan_dir_risks(dir: &Path) -> Result<Vec<String>, (StatusCode, Json<Value>)> {
    if !dir.is_dir() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("目录不存在：{}", dir.display()),
        ));
    }
    let manifest_path = dir.join("manifest.json");
    if !manifest_path.exists() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("缺少 manifest.json：{}", dir.display()),
        ));
    }
    let manifest =
        PluginManifest::load(&manifest_path).map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    let manifest_content = std::fs::read_to_string(&manifest_path).unwrap_or_default();
    let entry_content = manifest
        .entry
        .as_ref()
        .and_then(|entry| std::fs::read_to_string(dir.join(entry)).ok());
    Ok(scan_plugin_for_risks(
        &manifest_content,
        entry_content.as_deref(),
        &manifest.network_allowlist,
    ))
}

fn scan_summary(path: &Path, manifest: &PluginManifest) -> Vec<String> {
    let base = path.parent().unwrap_or(path);
    let manifest_content = std::fs::read_to_string(path).unwrap_or_default();
    let entry_content = manifest
        .entry
        .as_ref()
        .and_then(|entry| std::fs::read_to_string(base.join(entry)).ok());
    scan_plugin_for_risks(
        &manifest_content,
        entry_content.as_deref(),
        &manifest.network_allowlist,
    )
}

fn list_files(dir: &Path) -> Vec<String> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(list_files(&path));
            } else {
                files.push(path.display().to_string());
            }
        }
    }
    files.sort();
    files
}
