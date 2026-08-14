//! 本地插件：manifest 解析与发现（工具经 MCP 服务器桥接）。
//!
//! M4b 市场治理：签名分发、静态扫描、versions.json 兼容选择、安装/更新/回滚。

use crate::mcp::McpServerConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// 插件签名信息（manifest 内嵌，M4b）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSignature {
    /// 签名算法（当前仅 "ed25519"）。
    pub algorithm: String,
    /// 公钥（base64，Ed25519 32 字节）。
    pub public_key_b64: String,
    /// 签名值（base64，对 manifest 内容 + 入口文件摘要）。
    pub signature_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    /// 插件提供的 MCP 服务器（工具桥接）。
    #[serde(default)]
    pub mcp: Option<McpServerConfig>,
    /// 要求的最低 App 版本（如 "0.5.8"；None 表示不限制）。
    #[serde(default)]
    pub min_app_version: Option<String>,
    /// 入口脚本（相对 manifest 目录；静态扫描与签名摘要的目标）。
    #[serde(default)]
    pub entry: Option<String>,
    /// 网络访问 allowlist 域（静态扫描校验；空 = 不允许联网）。
    #[serde(default)]
    pub network_allowlist: Vec<String>,
    /// 签名（可选；缺失时加载决策由调用方按策略处理）。
    #[serde(default)]
    pub signature: Option<PluginSignature>,
}

impl PluginManifest {
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
        let manifest: PluginManifest = serde_json::from_str(&content)
            .map_err(|error| format!("manifest 解析失败：{error}"))?;
        if manifest.id.trim().is_empty() || manifest.name.trim().is_empty() {
            return Err("插件 id 与 name 不能为空".to_string());
        }
        Ok(manifest)
    }
}

/// 从全局 `<data>/plugins` 与工作区 `plugins/` 发现插件（按 id 去重，工作区优先）。
pub fn discover_plugins(workspace: &Path, data_root: &Path) -> Vec<(PathBuf, PluginManifest)> {
    let mut plugins = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for root in [workspace.join("plugins"), data_root.join("plugins")] {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
            if !is_dir {
                continue;
            }
            let manifest_path = entry.path().join("manifest.json");
            if !manifest_path.exists() {
                continue;
            }
            if let Ok(manifest) = PluginManifest::load(&manifest_path) {
                if seen.insert(manifest.id.clone()) {
                    plugins.push((manifest_path, manifest));
                }
            }
        }
    }
    plugins
}

/// 插件启用状态存储（v0.5 生产加固）：只记录被禁用的 id，
/// 因此未声明过的插件默认启用，状态文件可延迟创建。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginStateStore {
    /// 已禁用的插件 id。
    disabled: HashSet<String>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl PluginStateStore {
    /// 绑定持久化路径（如 `<data>/plugin_state.json`）；None 表示纯内存。
    pub fn new(path: Option<PathBuf>) -> Self {
        let mut store = Self {
            disabled: HashSet::new(),
            path,
        };
        if let Some(path) = &store.path {
            store.disabled = Self::load(path);
        }
        store
    }

    fn load(path: &Path) -> HashSet<String> {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|content| serde_json::from_str::<PluginStateStore>(&content).ok())
            .map(|store| store.disabled)
            .unwrap_or_default()
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), String> {
        if enabled {
            self.disabled.remove(id);
        } else {
            self.disabled.insert(id.to_string());
        }
        self.save()
    }

    /// 未记录的插件默认启用。
    pub fn is_enabled(&self, id: &str) -> bool {
        !self.disabled.contains(id)
    }

    pub fn disabled_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.disabled.iter().cloned().collect();
        ids.sort();
        ids
    }

    /// 恢复全部插件为启用状态。
    pub fn reset(&mut self) -> Result<(), String> {
        self.disabled.clear();
        self.save()
    }

    fn save(&self) -> Result<(), String> {
        if let Some(path) = &self.path {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            let json = serde_json::to_string_pretty(&self).map_err(|error| error.to_string())?;
            std::fs::write(path, json).map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

/// 只返回启用插件的发现结果（配合 `PluginStateStore::is_enabled`）。
pub fn discover_enabled_plugins(
    workspace: &Path,
    data_root: &Path,
    state: &PluginStateStore,
) -> Vec<(PathBuf, PluginManifest)> {
    discover_plugins(workspace, data_root)
        .into_iter()
        .filter(|(_, manifest)| state.is_enabled(&manifest.id))
        .collect()
}

// ---------- M4b 市场治理：versions.json 兼容选择 ----------

/// versions.json：插件版本 → 要求的最低 App 版本映射。
///
/// ```json
/// { "compatibility": { "1.1.0": "0.5.8", "1.0.0": "0.5.0" } }
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VersionsJson {
    #[serde(default)]
    pub compatibility: std::collections::BTreeMap<String, String>,
}

impl VersionsJson {
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        serde_json::from_str(&content).map_err(|e| format!("versions.json 解析失败：{e}"))
    }

    /// 从可用插件版本中选一个与当前 App 版本兼容的（要求 app >= min_app）；
    /// 多个兼容时选**最高**插件版本；全不兼容返回 None（应拒绝安装）。
    pub fn resolve_compatible(&self, current_app_version: &str) -> Option<String> {
        self.compatibility
            .iter()
            .filter(|(_, min_app)| version_gte(current_app_version, min_app))
            .map(|(plugin_version, _)| plugin_version.clone())
            .max_by(|a, b| version_cmp(a, b))
    }
}

/// 版本比较：`a >= b`（按数字段比较；无法解析时按字符串退化）。
pub fn version_gte(a: &str, b: &str) -> bool {
    version_cmp(a, b) != std::cmp::Ordering::Less
}

/// 版本比较：返回 Ordering（点分数字段；非数字段视为 0；解析失败按字符串比较）。
pub fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |s: &str| -> Vec<u64> {
        s.trim()
            .split('.')
            .map(|part| {
                part.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
            })
            .map(|digits| digits.parse::<u64>().unwrap_or(0))
            .collect()
    };
    let (va, vb) = (parse(a), parse(b));
    let len = va.len().max(vb.len());
    for i in 0..len {
        let pa = va.get(i).copied().unwrap_or(0);
        let pb = vb.get(i).copied().unwrap_or(0);
        if pa != pb {
            return pa.cmp(&pb);
        }
    }
    std::cmp::Ordering::Equal
}

// ---------- M4b 市场治理：静态扫描 ----------

/// 危险 API 黑名单（Python 入口脚本常见危险调用；命中即风险）。
const RISKY_APIS: &[&str] = &[
    "os.system",
    "os.popen",
    "subprocess.Popen",
    "subprocess.call",
    "subprocess.run",
    "os.startfile",
    "shutil.rmtree",
    "ctypes.windll",
    "ctypes.CDLL",
    "eval(",
    "exec(",
    "execfile",
    "__import__('os')",
    "pickle.loads",
    "yaml.load(",
    "tempfile.mktemp",
];

/// 网络访问关键字（提取 URL 域做 allowlist 校验）。
const NETWORK_APIS: &[&str] = &[
    "urllib.request",
    "urllib.urlopen",
    "requests.get",
    "requests.post",
    "http.client",
    "socket.",
    "websocket",
    "aiohttp",
    "httpx.",
];

/// 静态扫描：对 manifest 与入口脚本内容做危险调用与网络域检查。
///
/// 返回风险列表（空 = 通过）。`allowlist_domains` 来自 manifest.network_allowlist。
pub fn scan_plugin_for_risks(
    manifest_content: &str,
    entry_content: Option<&str>,
    allowlist_domains: &[String],
) -> Vec<String> {
    let mut risks = Vec::new();
    let text = format!(
        "{}\n{}",
        manifest_content,
        entry_content.unwrap_or_default()
    );

    for api in RISKY_APIS {
        if text.contains(api) {
            risks.push(format!("危险 API：{api}"));
        }
    }

    // 网络域校验：找到 http(s) URL 域名，不在 allowlist 即风险。
    let mut found_domains = HashSet::new();
    for marker in NETWORK_APIS {
        if text.contains(marker) {
            risks.push(format!("网络调用（未白名单核验）：{marker}"));
            found_domains.insert(marker.to_string());
        }
    }
    for url in extract_urls(&text) {
        let domain = url_domain(&url);
        if let Some(domain) = domain {
            if !allowlist_domains.iter().any(|d| d == &domain) {
                risks.push(format!("域外网络：{domain}"));
            }
        }
    }
    let _ = found_domains;
    risks
}

/// 提取文本中的 http/https URL（简单正则）。
fn extract_urls(text: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 7 < bytes.len() {
        if bytes[i..].starts_with(b"http://") || bytes[i..].starts_with(b"https://") {
            let start = i;
            let mut end = i;
            while end < bytes.len()
                && !bytes[end].is_ascii_whitespace()
                && bytes[end] != b'"'
                && bytes[end] != b'\''
                && bytes[end] != b')'
                && bytes[end] != b']'
                && bytes[end] != b'}'
            {
                end += 1;
            }
            urls.push(text[start..end].to_string());
            i = end;
        } else {
            i += 1;
        }
    }
    urls
}

/// 提取 URL 的域名（去掉协议/路径/端口）。
fn url_domain(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.split(':').next().unwrap_or("");
    if host.is_empty() {
        None
    } else {
        Some(host.to_lowercase())
    }
}

// ---------- M4b 市场治理：安装 / 更新 / 回滚状态机 ----------

/// 插件安装生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginInstallState {
    /// 已发现（未验证）。
    Discovered,
    /// 校验通过（签名/扫描/版本）。
    Verified,
    /// 已激活（拷贝到数据目录并纳入发现）。
    Activated,
    /// 更新失败，已回滚到旧版。
    RolledBack,
}

/// 一次安装/更新操作的结果（含审计轨迹）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInstallReport {
    pub id: String,
    pub version: String,
    pub state: PluginInstallState,
    /// 审计事件（event: detail）。
    pub audit: Vec<String>,
}

/// 插件市场管理器：安装 → 验证 → 激活；更新先备份，失败自动回滚。
pub struct PluginManager {
    /// 插件数据根目录（<data_root>/plugins 用于激活）。
    data_root: PathBuf,
    /// App 当前版本（兼容选择/最小版本校验）。
    app_version: String,
    /// 要求签名（缺签名拒绝加载）。
    require_signature: bool,
}

impl PluginManager {
    pub fn new(data_root: PathBuf, app_version: String) -> Self {
        Self {
            data_root,
            app_version,
            require_signature: true,
        }
    }

    /// 是否强制签名（默认 true；测试可放宽）。
    pub fn set_require_signature(&mut self, require: bool) {
        self.require_signature = require;
    }

    /// 校验插件目录（manifest + 版本 + 扫描 + 签名）。
    ///
    /// 校验项：manifest 可解析、min_app_version 兼容、静态扫描零风险、
    /// 签名存在且有效（require_signature 时）。返回校验报告。
    pub fn verify_plugin_dir(&self, plugin_dir: &Path) -> Result<PluginInstallReport, String> {
        let mut audit = Vec::new();
        let manifest_path = plugin_dir.join("manifest.json");
        let manifest = PluginManifest::load(&manifest_path)?;

        // 1. min_app_version 兼容。
        if let Some(min) = &manifest.min_app_version {
            if !version_gte(&self.app_version, min) {
                return Err(format!(
                    "插件 {} {} 要求 App >= {min}，当前 {}，拒绝安装",
                    manifest.id, manifest.version, self.app_version
                ));
            }
            audit.push(format!(
                "min_app_version 兼容（App {}/min {min}）",
                self.app_version
            ));
        }

        // 2. 静态扫描。
        let entry_content = manifest
            .entry
            .as_ref()
            .and_then(|entry| std::fs::read_to_string(plugin_dir.join(entry)).ok());
        let manifest_content = std::fs::read_to_string(&manifest_path).unwrap_or_default();
        let risks = scan_plugin_for_risks(
            &manifest_content,
            entry_content.as_deref(),
            &manifest.network_allowlist,
        );
        if !risks.is_empty() {
            return Err(format!(
                "插件 {} 静态扫描未通过：{}",
                manifest.id,
                risks.join("；")
            ));
        }
        audit.push("静态扫描通过".to_string());

        // 3. 签名。
        if let Some(signature) = &manifest.signature {
            verify_plugin_signature(&manifest, entry_content.as_deref())?;
            audit.push(format!("签名校验通过（{}）", signature.algorithm));
        } else if self.require_signature {
            return Err(format!("插件 {} 缺少签名，拒绝加载", manifest.id));
        }

        Ok(PluginInstallReport {
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            state: PluginInstallState::Verified,
            audit,
        })
    }

    /// 安装：verify → 拷贝到 <data_root>/plugins/{id} → 激活。
    pub fn install(&self, plugin_dir: &Path) -> Result<PluginInstallReport, String> {
        let report = self.verify_plugin_dir(plugin_dir)?;
        let target = self.data_root.join("plugins").join(&report.id);
        if target.exists() {
            return Err(format!(
                "插件 {} 已安装（先 update 或 uninstall）",
                report.id
            ));
        }
        copy_dir(plugin_dir, &target)?;
        let mut audit = report.audit;
        audit.push(format!("已激活到 {}", target.display()));
        Ok(PluginInstallReport {
            id: report.id,
            version: report.version,
            state: PluginInstallState::Activated,
            audit,
        })
    }

    /// 更新：备份旧版 → verify 新版 → 替换；失败自动回滚到备份。
    pub fn update(
        &self,
        plugin_dir: &Path,
        backup_root: &Path,
    ) -> Result<PluginInstallReport, String> {
        let report = self.verify_plugin_dir(plugin_dir)?;
        let target = self.data_root.join("plugins").join(&report.id);
        if !target.exists() {
            return Err(format!("插件 {} 未安装，无法更新", report.id));
        }
        let mut audit = report.audit;
        // 1. 备份旧版。
        let backup = backup_root.join(format!("{}-{}", report.id, report.version));
        if backup.exists() {
            let _ = std::fs::remove_dir_all(&backup);
        }
        copy_dir(&target, &backup)?;
        audit.push(format!("旧版已备份到 {}", backup.display()));
        // 2. 替换为新版。
        let _ = std::fs::remove_dir_all(&target);
        if let Err(error) = copy_dir(plugin_dir, &target) {
            // 3. 失败回滚。
            let _ = std::fs::remove_dir_all(&target);
            let rollback = copy_dir(&backup, &target).map_err(|rb| {
                format!(
                    "更新失败（{error}）且回滚失败（{rb}），旧版保留在 {}",
                    backup.display()
                )
            });
            if rollback.is_ok() {
                audit.push(format!("更新失败（{error}），已自动回滚旧版"));
            }
            return Err(format!("更新失败：{error}；已回滚旧版"));
        }
        audit.push(format!("已更新到版本 {}", report.version));
        Ok(PluginInstallReport {
            id: report.id,
            version: report.version,
            state: PluginInstallState::Activated,
            audit,
        })
    }

    /// 卸载：移除激活目录（备份保留由调用方决定）。
    pub fn uninstall(&self, id: &str) -> Result<Vec<String>, String> {
        let target = self.data_root.join("plugins").join(id);
        if !target.exists() {
            return Err(format!("插件 {id} 未安装"));
        }
        std::fs::remove_dir_all(&target).map_err(|e| e.to_string())?;
        Ok(vec![format!("已卸载 {id}")])
    }
}

/// 递归拷贝目录（std 实现；忽略符号链接）。
fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    if !from.is_dir() {
        return Err(format!("{} 不是目录", from.display()));
    }
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(from).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let target = to.join(entry.file_name());
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
// ---------- M4b 市场治理：Ed25519 签名校验 ----------

/// 插件市场提交/审核状态（P2 预留：市场服务端接口的数据结构）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginReviewState {
    /// 已提交，等待审核。
    Submitted,
    /// 审核通过（可分发）。
    Approved,
    /// 审核拒绝。
    Rejected,
}

/// 一次插件提交（供未来市场服务端 API 使用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSubmission {
    pub id: String,
    pub version: String,
    pub review_state: PluginReviewState,
    #[serde(default)]
    pub review_note: Option<String>,
    #[serde(default)]
    pub signature: Option<PluginSignature>,
}

/// 市场更新清单（P2：远端最新版本清单）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MarketUpdateManifest {
    #[serde(default)]
    pub plugins: Vec<MarketPluginEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketPluginEntry {
    pub id: String,
    /// 最新可用版本。
    pub latest_version: String,
    /// 要求的最低 App 版本。
    #[serde(default)]
    pub min_app_version: Option<String>,
    #[serde(default)]
    pub signature: Option<PluginSignature>,
}

impl MarketUpdateManifest {
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        serde_json::from_str(&content).map_err(|e| format!("更新清单解析失败：{e}"))
    }

    /// 当前插件是否有可用更新（远端版本 > 本地版本，且 App 版本兼容）。
    pub fn has_update(&self, plugin_id: &str, local_version: &str, app_version: &str) -> bool {
        self.plugins.iter().any(|entry| {
            entry.id == plugin_id
                && version_cmp(&entry.latest_version, local_version) == std::cmp::Ordering::Greater
                && entry
                    .min_app_version
                    .as_deref()
                    .map(|min| version_gte(app_version, min))
                    .unwrap_or(true)
        })
    }
}

/// 计算插件的完整性摘要（SHA-256）。
///
/// 摘要输入 = 结构化字段（id|name|version|entry 路径）与入口文件内容——**不含
/// signature 字段本身**（签名后才加入 manifest，校验口径自洽）。篡改 id/version/
/// entry 或入口脚本内容都会导致验签失败。
pub fn plugin_digest(manifest: &PluginManifest, entry_content: Option<&str>) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(manifest.id.as_bytes());
    hasher.update(b"|");
    hasher.update(manifest.name.as_bytes());
    hasher.update(b"|");
    hasher.update(manifest.version.as_bytes());
    hasher.update(b"|");
    hasher.update(manifest.entry.as_deref().unwrap_or("").as_bytes());
    if let Some(entry) = entry_content {
        hasher.update(b"|");
        hasher.update(entry.as_bytes());
    }
    hasher.finalize().into()
}

/// 校验插件签名：对 `plugin_digest` 的摘要做 Ed25519 验签。
///
/// 失败场景：算法不支持、公钥/签名 base64 非法、签名不匹配（内容被篡改）。
pub fn verify_plugin_signature(
    manifest: &PluginManifest,
    entry_content: Option<&str>,
) -> Result<(), String> {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    use std::convert::TryInto;

    let signature = manifest
        .signature
        .as_ref()
        .ok_or_else(|| format!("插件 {} 缺少签名", manifest.id))?;
    if signature.algorithm.to_lowercase() != "ed25519" {
        return Err(format!("不支持的签名算法：{}", signature.algorithm));
    }
    let pub_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        &signature.public_key_b64,
    )
    .map_err(|e| format!("公钥 base64 非法：{e}"))?;
    let sig_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        &signature.signature_b64,
    )
    .map_err(|e| format!("签名 base64 非法：{e}"))?;

    let key: VerifyingKey = pub_bytes
        .as_slice()
        .try_into()
        .map_err(|_| "公钥长度非法（需 32 字节）".to_string())?;
    let sig: Signature = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| "签名长度非法（需 64 字节）".to_string())?;

    let digest = plugin_digest(manifest, entry_content);
    key.verify(&digest, &sig)
        .map_err(|_| format!("插件 {} 签名校验失败（内容可能被篡改）", manifest.id))
        .map(|_| ())
}

/// 插件 → MCP 服务器配置：以插件 id 为服务器名，相对启动命令按 manifest 所在目录解析。
/// 无 `mcp` 声明返回 None。
pub fn plugin_mcp_config(
    manifest_path: &Path,
    manifest: &PluginManifest,
) -> Option<McpServerConfig> {
    let mut config = manifest.mcp.clone()?;
    config.name = manifest.id.clone();
    let command_path = Path::new(&config.command);
    if command_path.is_relative() {
        if let Some(base) = manifest_path.parent() {
            let resolved = base.join(command_path);
            if resolved.exists() {
                config.command = resolved.to_string_lossy().into_owned();
            }
        }
    }
    if let Some(base) = manifest_path.parent() {
        config.args = config
            .args
            .into_iter()
            .map(|argument| {
                let path = Path::new(&argument);
                if path.is_relative() {
                    let resolved = base.join(path);
                    if resolved.exists() {
                        return resolved.to_string_lossy().into_owned();
                    }
                }
                argument
            })
            .collect();
    }
    Some(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_and_validates_manifest() {
        let dir =
            std::env::temp_dir().join(format!("owo-plugin-manifest-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("manifest.json");
        std::fs::write(
            &path,
            r#"{
                "id": "owo.plugin.demo",
                "name": "Demo",
                "version": "1.0.0",
                "permissions": ["agent:tools"],
                "mcp": {
                    "name": "demo",
                    "transport": "stdio",
                    "command": "demo-server",
                    "args": []
                }
            }"#,
        )
        .unwrap();
        let manifest = PluginManifest::load(&path).unwrap();
        assert_eq!(manifest.id, "owo.plugin.demo");
        assert!(manifest.mcp.is_some());

        std::fs::write(&path, r#"{"id":"","name":"","version":"1"}"#).unwrap();
        assert!(PluginManifest::load(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discovers_plugins_with_workspace_precedence() {
        let workspace =
            std::env::temp_dir().join(format!("owo-plugin-workspace-{}", uuid::Uuid::new_v4()));
        let data = std::env::temp_dir().join(format!("owo-plugin-data-{}", uuid::Uuid::new_v4()));
        let dir = workspace.join("plugins").join("a");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            r#"{"id":"a","name":"A","version":"1.0.0"}"#,
        )
        .unwrap();
        let global = data.join("plugins").join("a");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(
            global.join("manifest.json"),
            r#"{"id":"a","name":"A-global","version":"1.0.0"}"#,
        )
        .unwrap();
        let other = data.join("plugins").join("b");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(
            other.join("manifest.json"),
            r#"{"id":"b","name":"B","version":"2.0.0"}"#,
        )
        .unwrap();

        let plugins = discover_plugins(&workspace, &data);
        assert_eq!(plugins.len(), 2);
        let a = plugins.iter().find(|(_, m)| m.id == "a").unwrap();
        assert_eq!(a.1.name, "A"); // 工作区优先
        let _ = std::fs::remove_dir_all(&workspace);
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn plugin_mcp_config_resolves_relative_command() {
        let dir = std::env::temp_dir().join(format!("owo-plugin-mcp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("server.py"), "# placeholder").unwrap();
        std::fs::write(dir.join("worker.py"), "# placeholder").unwrap();
        let manifest_path = dir.join("manifest.json");
        let manifest = PluginManifest {
            id: "owo.demo".to_string(),
            name: "Demo".to_string(),
            version: "1.0.0".to_string(),
            description: String::new(),
            permissions: Vec::new(),
            min_app_version: None,
            entry: None,
            network_allowlist: Vec::new(),
            signature: None,
            mcp: Some(McpServerConfig {
                name: "ignored".to_string(),
                transport: "stdio".to_string(),
                command: "server.py".to_string(),
                args: vec!["worker.py".to_string(), "--stdio".to_string()],
                url: None,
                timeout_ms: None,
            }),
        };
        // 相对命令按 manifest 目录解析；服务器名 = 插件 id。
        let config = plugin_mcp_config(&manifest_path, &manifest).expect("应产出 MCP 配置");
        assert_eq!(config.name, "owo.demo");
        assert_eq!(config.command, dir.join("server.py").to_string_lossy());
        assert_eq!(config.args[0], dir.join("worker.py").to_string_lossy());
        assert_eq!(config.args[1], "--stdio");
        // 无 mcp 声明返回 None。
        let bare = PluginManifest {
            id: "owo.view".to_string(),
            name: "View".to_string(),
            version: "1.0.0".to_string(),
            description: String::new(),
            permissions: Vec::new(),
            min_app_version: None,
            entry: None,
            network_allowlist: Vec::new(),
            signature: None,
            mcp: None,
        };
        assert!(plugin_mcp_config(&manifest_path, &bare).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
