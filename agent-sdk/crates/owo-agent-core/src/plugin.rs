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

/// 沙箱门卫：插件宿主（MCP 服务器）统一经 SandboxManager 授权（X01）。
/// 策略：只读系统作用域 + 网络白名单（manifest.network_allowlist 或回环）+ Job 级隔离；
/// 数据出境开关（R9）：关闭时插件一律不得出网（HTTP MCP 拒绝，stdio 强制回环）。
/// 拒绝时产生审计事件（EgressRejected）并返回显式错误。
fn sandbox_gate_for_mcp(
    manifest: &PluginManifest,
    mcp: &McpServerConfig,
    egress_enabled: bool,
) -> Result<(), String> {
    use crate::sandbox::{
        default_manager, FileScope, IsolationLevel, NetworkPolicy, SandboxCommand, SandboxPolicy,
    };
    let reject_and_audit = |detail: String| -> Result<(), String> {
        let manager = default_manager();
        let mut manager = manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        manager.record_egress_rejection(format!("plugin:{}", manifest.id), detail.clone());
        Err(detail)
    };
    if mcp.transport == "http" {
        if !egress_enabled {
            return reject_and_audit(format!(
                "插件 {} 的 HTTP MCP 被数据出境开关拒绝（egress 关闭）",
                manifest.id
            ));
        }
        // HTTP MCP 硬化：URL host 必须在 manifest.network_allowlist 内，否则拒绝。
        let url = mcp
            .url
            .as_deref()
            .ok_or_else(|| "HTTP MCP 服务器缺少 url".to_string())?;
        let domain = url_domain(url).ok_or_else(|| format!("HTTP MCP URL 无法解析域名：{url}"))?;
        if manifest.network_allowlist.is_empty() {
            return reject_and_audit(format!(
                "插件 {} 的 HTTP MCP 目标 {domain} 不在网络白名单（network_allowlist 为空），拒绝",
                manifest.id
            ));
        }
        let allowed = manifest.network_allowlist.iter().any(|entry| {
            let entry_domain = if entry.starts_with("http://") || entry.starts_with("https://") {
                url_domain(entry).unwrap_or_default()
            } else {
                entry.split(':').next().unwrap_or(entry).to_lowercase()
            };
            entry_domain == domain
        });
        if !allowed {
            return reject_and_audit(format!(
                "插件 {} 的 HTTP MCP 目标 {domain} 不在网络白名单（{}），拒绝",
                manifest.id,
                manifest.network_allowlist.join("、")
            ));
        }
        return Ok(());
    }
    let policy = SandboxPolicy {
        name: format!("plugin:{}", manifest.id),
        file_scope: FileScope::WorkspacePlusReadonlySystem,
        // egress 关闭：出网白名单失效，强制回环（默认 deny 网络）。
        network_policy: if !egress_enabled || manifest.network_allowlist.is_empty() {
            NetworkPolicy::Loopback
        } else {
            NetworkPolicy::AllowList
        },
        allow_hosts: if egress_enabled {
            manifest.network_allowlist.clone()
        } else {
            Vec::new()
        },
        cpu_ms: None,
        mem_mb: Some(1024),
        ttl_secs: None,
        require_isolation: IsolationLevel::JobOnly,
        allow_degraded: true,
        // 插件宿主可能需要子进程（如剪贴板插件调 powershell）。
        active_process_limit: Some(32),
        ..SandboxPolicy::default()
    };
    let sandbox_command = SandboxCommand::new(&mcp.command, policy).with_args(mcp.args.clone());
    let manager = default_manager();
    let mut manager = manager
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    manager
        .guard(&sandbox_command)
        .map_err(|error| format!("插件宿主沙箱拒绝（{}）：{error}", mcp.command))?;
    Ok(())
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
    /// 数据出境开关（R9：关闭时插件网络白名单失效，一切出网请求拒绝并审计）。
    egress_enabled: bool,
    /// 吊销列表（R10：id@version 命中即拒绝加载）。
    revocations: Vec<PluginRevocation>,
    /// 官方插件豁免白名单（R10：仅对有效签名官方包生效的扫描风险豁免）。
    official_ids: std::collections::HashSet<String>,
}

impl PluginManager {
    pub fn new(data_root: PathBuf, app_version: String) -> Self {
        Self {
            data_root,
            app_version,
            require_signature: true,
            egress_enabled: true,
            revocations: Vec::new(),
            official_ids: std::collections::HashSet::new(),
        }
    }

    /// 是否强制签名（默认 true；测试可放宽）。
    pub fn set_require_signature(&mut self, require: bool) {
        self.require_signature = require;
    }

    /// 数据出境开关（默认 true；关闭后插件宿主不得出网）。
    pub fn set_egress_enabled(&mut self, enabled: bool) {
        self.egress_enabled = enabled;
    }

    pub fn egress_enabled(&self) -> bool {
        self.egress_enabled
    }

    /// 从吊销列表文件加载（`<data_root>/revocations.json` 或自定义路径；文件缺失=空列表）。
    pub fn load_revocations(&mut self, path: Option<&Path>) {
        let path = path
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.data_root.join("revocations.json"));
        self.revocations = std::fs::read_to_string(&path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default();
    }

    pub fn revocations(&self) -> &[PluginRevocation] {
        &self.revocations
    }

    /// 追加吊销（id+version；全版本用 `*`）。
    pub fn add_revocation(
        &mut self,
        id: impl Into<String>,
        version: impl Into<String>,
        reason: impl Into<String>,
    ) {
        let revocation = PluginRevocation {
            id: id.into(),
            version: version.into(),
            reason: reason.into(),
            ts: chrono::Utc::now().to_rfc3339(),
        };
        if !self
            .revocations
            .iter()
            .any(|entry| entry.id == revocation.id && entry.version == revocation.version)
        {
            self.revocations.push(revocation);
        }
    }

    /// 保存吊销列表到 `<data_root>/revocations.json`。
    pub fn save_revocations(&self) -> Result<(), String> {
        let path = self.data_root.join("revocations.json");
        std::fs::create_dir_all(&self.data_root).map_err(|e| e.to_string())?;
        let content = serde_json::to_string_pretty(&self.revocations).map_err(|e| e.to_string())?;
        std::fs::write(path, content).map_err(|e| e.to_string())
    }

    /// 清空吊销列表（测试/管理用）。
    pub fn clear_revocations(&mut self) {
        self.revocations.clear();
    }

    /// 是否被吊销（version `*` 匹配全部版本）。
    pub fn is_revoked(&self, id: &str, version: &str) -> bool {
        self.revocations
            .iter()
            .any(|entry| entry.id == id && (entry.version == "*" || entry.version == version))
    }

    /// 官方豁免白名单（R10：仅对有效签名官方包生效）。
    pub fn set_official_allowlist(&mut self, ids: &[String]) {
        self.official_ids = ids.iter().cloned().collect();
    }

    fn is_official_and_signed(
        &self,
        manifest: &PluginManifest,
        entry_content: Option<&str>,
    ) -> bool {
        if !self.official_ids.contains(&manifest.id) {
            return false;
        }
        match &manifest.signature {
            Some(signature) => {
                verify_plugin_signature(manifest, entry_content).is_ok()
                    && signature.algorithm == "ed25519"
            }
            None => false,
        }
    }

    /// 校验插件目录（manifest + 版本 + 扫描 + 签名）。
    ///
    /// 校验项：manifest 可解析、min_app_version 兼容、静态扫描零风险、
    /// 签名存在且有效（require_signature 时）。返回校验报告。
    pub fn verify_plugin_dir(&self, plugin_dir: &Path) -> Result<PluginInstallReport, String> {
        let mut audit = Vec::new();
        let manifest_path = plugin_dir.join("manifest.json");
        let manifest = PluginManifest::load(&manifest_path)?;

        // 0. 吊销检查（R10）：id+version 命中吊销列表 → 拒绝加载。
        if self.is_revoked(&manifest.id, &manifest.version) {
            record_plugin_rejection(&manifest, "命中吊销列表".to_string());
            return Err(format!(
                "插件 {} {} 已被吊销，拒绝加载",
                manifest.id, manifest.version
            ));
        }

        // 0.5 路径校验（R10：zip-slip 变体——entry/mcp 引用与目录内文件不得含
        // `..` 组件或绝对路径，拷贝前拦截）。
        validate_plugin_paths(&manifest, plugin_dir)?;
        audit.push("路径校验通过（无 zip-slip）".to_string());

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
            // 官方豁免（R10）：仅对签名官方包生效——风险项记录为豁免而非拒绝。
            let official_signed = self.is_official_and_signed(&manifest, entry_content.as_deref());
            if official_signed {
                audit.push(format!("官方插件豁免风险项：{}", risks.join("；")));
            } else {
                record_plugin_rejection(&manifest, format!("静态扫描未通过：{}", risks.join("；")));
                return Err(format!(
                    "插件 {} 静态扫描未通过：{}",
                    manifest.id,
                    risks.join("；")
                ));
            }
        } else {
            audit.push("静态扫描通过".to_string());
        }
        // 依赖清单（R10：入口脚本 import/require 记录，低风险）。
        if let Some(deps) = extract_dependencies(entry_content.as_deref()) {
            audit.push(format!("依赖清单：{}", deps.join("、")));
        }

        // 3. 签名。
        if let Some(signature) = &manifest.signature {
            verify_plugin_signature(&manifest, entry_content.as_deref())?;
            audit.push(format!("签名校验通过（{}）", signature.algorithm));
        } else if self.require_signature {
            record_plugin_rejection(&manifest, "缺少签名".to_string());
            return Err(format!("插件 {} 缺少签名，拒绝加载", manifest.id));
        }

        // 4. 沙箱门卫（X01）：插件宿主（MCP 服务器/入口）统一经 SandboxManager 授权。
        if let Some(mcp) = &manifest.mcp {
            sandbox_gate_for_mcp(&manifest, mcp, self.egress_enabled)?;
            audit.push("插件宿主沙箱策略通过".to_string());
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

/// 递归拷贝目录（std 实现；忽略符号链接；R10：逐文件拒绝 `..` 组件/绝对路径/符号链接，
/// 防 zip-slip 变体）。
fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    if !from.is_dir() {
        return Err(format!("{} 不是目录", from.display()));
    }
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(from).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == ".." || name.contains('/') || name.contains('\\') {
            return Err(format!("非法路径组件：{name}（zip-slip 拒绝）"));
        }
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        if file_type.is_symlink() {
            return Err(format!("插件目录含符号链接：{name}（拒绝）"));
        }
        let target = to.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// 插件吊销记录（R10）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginRevocation {
    pub id: String,
    /// 版本（`*` = 全部版本）。
    pub version: String,
    pub reason: String,
    pub ts: String,
}

/// 路径校验（R10）：manifest 引用的 entry/MCP 命令路径与目录内文件
/// 不得含 `..` 组件或绝对路径（zip-slip 变体拦截）。
fn validate_plugin_paths(manifest: &PluginManifest, plugin_dir: &Path) -> Result<(), String> {
    if let Some(entry) = &manifest.entry {
        let entry_path = Path::new(entry);
        if entry_path.is_absolute()
            || entry_path
                .components()
                .any(|component| component == std::path::Component::ParentDir)
        {
            return Err(format!("插件入口路径非法（zip-slip）：{entry}"));
        }
        if !plugin_dir.join(entry_path).is_file() {
            return Err(format!("插件入口不存在：{entry}"));
        }
    }
    // 目录内文件路径遍历检查（拷贝前拦截绝对/父级路径）。
    let mut pending = vec![plugin_dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
        for entry in entries.flatten() {
            let file_type = entry.file_type().map_err(|e| e.to_string())?;
            if file_type.is_symlink() {
                return Err(format!(
                    "插件含符号链接：{}（拒绝）",
                    entry.path().display()
                ));
            }
            if file_type.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(())
}

/// 入口脚本依赖提取（R10：import/require 记录，低风险）。
pub fn extract_dependencies(entry_content: Option<&str>) -> Option<Vec<String>> {
    let content = entry_content?;
    let mut deps: Vec<String> = Vec::new();
    let mut rest = content;
    while !rest.is_empty() {
        let rel_index = rest.find("require(");
        let from_index = rest.find("from '");
        let next = match (rel_index, from_index) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => break,
        };
        let candidate = &rest[next..];
        let dep = if let Some(rest) = candidate.strip_prefix("require(") {
            let quoted = rest.trim_start();
            quoted
                .strip_prefix('\'')
                .and_then(|s| s.split('\'').next())
                .or_else(|| quoted.strip_prefix('"').and_then(|s| s.split('"').next()))
        } else if let Some(rest) = candidate.strip_prefix("from '") {
            rest.split('\'').next()
        } else {
            None
        };
        if let Some(dep) = dep {
            if !dep.is_empty() && !deps.iter().any(|existing| existing == dep) {
                deps.push(dep.to_string());
            }
        }
        rest = &candidate[1..];
    }
    if deps.is_empty() {
        None
    } else {
        Some(deps)
    }
}

/// 插件拒绝审计（R10）：经全局沙箱管理器记录 PluginRejected 事件（可汇入审计链）。
fn record_plugin_rejection(manifest: &PluginManifest, detail: String) {
    let manager = crate::sandbox::default_manager();
    let mut manager = manager
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    manager.record_plugin_rejection(
        format!("plugin:{}@{}", manifest.id, manifest.version),
        detail,
    );
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
                network_allowlist: Vec::new(),
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
