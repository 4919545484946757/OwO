//! 远端插件市场客户端（Agent 2 子任务 1）。
//!
//! - 从 `OWO_MARKET_URL`（或入参 url）拉取 `registry.json`（reqwest + 超时）；
//!   无 URL 时回退本地 `market.json`（离线模式）。
//! - 下载插件 zip → 临时目录 → **zip-slip 防护解包**（拒绝 `..` 与绝对路径）→
//!   `verify_plugin_signature` 强制校验 → `PluginManager::install/update`，全程审计。
//! - 签名失败 / zip-slip / 高危扫描均返回明确错误（HTTP 层映射 400）。

use owo_agent_core::plugin::{MarketUpdateManifest, PluginManager};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// registry 来源。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum RegistrySource {
    Remote,
    Local,
}

/// 拉取结果：registry 条目 + 来源。
pub struct FetchedRegistry {
    pub source: RegistrySource,
    pub manifest: MarketUpdateManifest,
}

/// 拉取 registry：优先入参 url → 环境变量 OWO_MARKET_URL → 本地 market.json。
pub async fn fetch_registry(
    url: Option<&str>,
    data_root: &Path,
) -> Result<FetchedRegistry, String> {
    if let Some(url) = url.filter(|u| !u.trim().is_empty()) {
        return fetch_remote(url).await;
    }
    if let Ok(env_url) = std::env::var("OWO_MARKET_URL") {
        if !env_url.trim().is_empty() {
            return fetch_remote(&env_url).await;
        }
    }
    // 离线回退：本地 market.json。
    let path = data_root.join("plugins").join("market.json");
    let manifest = MarketUpdateManifest::load(&path).map_err(|e| {
        format!("无远端 URL 且本地 market.json 不可用（{e}）；设置 OWO_MARKET_URL 或先 seed")
    })?;
    Ok(FetchedRegistry {
        source: RegistrySource::Local,
        manifest,
    })
}

/// 从远端拉取 registry.json（10s 超时）。
async fn fetch_remote(url: &str) -> Result<FetchedRegistry, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP 客户端构建失败：{e}"))?;
    let base = url.trim_end_matches('/');
    let registry_url = format!("{base}/registry.json");
    let response = client
        .get(&registry_url)
        .send()
        .await
        .map_err(|e| format!("拉取 registry 失败（{registry_url}）：{e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "registry 拉取返回 {}（{registry_url}）",
            response.status()
        ));
    }
    let manifest: MarketUpdateManifest = response
        .json()
        .await
        .map_err(|e| format!("registry.json 解析失败：{e}"))?;
    Ok(FetchedRegistry {
        source: RegistrySource::Remote,
        manifest,
    })
}

/// 下载插件 zip 到临时目录并解包（zip-slip 防护）。
///
/// 返回解包后的插件目录（临时目录由调用方持有生命周期）。
pub async fn download_and_unpack(url: &str, temp_root: &Path) -> Result<PathBuf, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("HTTP 客户端构建失败：{e}"))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("下载插件包失败（{url}）：{e}"))?;
    if !response.status().is_success() {
        return Err(format!("插件包下载返回 {}", response.status()));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("插件包读取失败：{e}"))?
        .to_vec();
    unpack_zip(&bytes, temp_root)
}

/// 解包 zip 字节到目录（zip-slip 防护：拒绝 `..`/绝对路径/盘符）。
pub fn unpack_zip(bytes: &[u8], dest: &Path) -> Result<PathBuf, String> {
    let reader = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader).map_err(|e| format!("zip 解析失败：{e}"))?;
    if archive.is_empty() {
        return Err("zip 包为空".to_string());
    }
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("zip 条目读取失败：{e}"))?;
        // zip-slip 防护：enclosed_name 返回 None 表示路径越界。
        let safe_path = entry
            .enclosed_name()
            .ok_or_else(|| format!("zip 条目路径越界（zip-slip 拦截）：{}", entry.name()))?;
        if safe_path.is_absolute() {
            return Err(format!("zip 条目为绝对路径（拦截）：{}", entry.name()));
        }
        let target = dest.join(&safe_path);
        if entry.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut out = std::fs::File::create(&target).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
        }
    }
    Ok(dest.to_path_buf())
}

/// 下载 + 解包 + 签名校验 + 扫描 + 安装/更新（远端安装闭环）。
///
/// `id`：registry 中的插件 id（用于匹配 registry 条目并定位下载 URL）。
/// 返回安装报告。签名失败/zip-slip/高危扫描返回明确错误。
pub async fn install_remote(
    data_root: &Path,
    registry: &FetchedRegistry,
    id: &str,
    version: Option<&str>,
    url: Option<&str>,
) -> Result<Value, String> {
    // 1. 定位 registry 条目。
    let entry = registry
        .manifest
        .plugins
        .iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| format!("registry 中无插件 {id}"))?;
    let selected_version = version.unwrap_or(&entry.latest_version);

    // 2. 构造下载 URL：{base}/plugins/{id}-{version}.zip。
    //    市场地址优先级：入参 url → OWO_MARKET_URL → 错误（要求明确传参）。
    let base = url
        .filter(|u| !u.trim().is_empty())
        .map(|u| u.trim_end_matches('/').to_string())
        .or_else(|| {
            std::env::var("OWO_MARKET_URL")
                .ok()
                .filter(|u| !u.trim().is_empty())
                .map(|u| u.trim_end_matches('/').to_string())
        })
        .ok_or_else(|| {
            format!("缺少市场地址：请传 url 或设置 OWO_MARKET_URL（插件 {id} v{selected_version}）")
        })?;
    let download_url = format!("{base}/plugins/{id}-{selected_version}.zip");

    // 3. 下载 + 解包到唯一临时目录（并行调用隔离）。
    let temp_root = std::env::temp_dir().join(format!(
        "owo-market-{id}-{selected_version}-{}",
        uuid::Uuid::new_v4()
    ));
    if temp_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_root);
    }
    std::fs::create_dir_all(&temp_root).map_err(|e| e.to_string())?;
    let plugin_dir = download_and_unpack(&download_url, &temp_root).await?;
    let manifest_path = plugin_dir.join("manifest.json");
    if !manifest_path.exists() {
        return Err("解包后缺少 manifest.json".to_string());
    }
    let manifest = owo_agent_core::plugin::PluginManifest::load(&manifest_path)?;
    if manifest.id != id {
        return Err(format!(
            "包内插件 id 不匹配（期望 {id}，实际 {}）",
            manifest.id
        ));
    }

    // 4. 签名强制校验（远端分发必须签名）。
    let entry_content = manifest
        .entry
        .as_ref()
        .and_then(|entry| std::fs::read_to_string(plugin_dir.join(entry)).ok());
    owo_agent_core::plugin::verify_plugin_signature(&manifest, entry_content.as_deref())?;

    // 5. 高危扫描。
    let manifest_content = std::fs::read_to_string(&manifest_path).unwrap_or_default();
    let risks = owo_agent_core::plugin::scan_plugin_for_risks(
        &manifest_content,
        entry_content.as_deref(),
        &manifest.network_allowlist,
    );
    if !risks.is_empty() {
        let _ = std::fs::remove_dir_all(&temp_root);
        return Err(format!("高危扫描未通过：{}", risks.join("；")));
    }

    // 6. 安装（已安装则更新）。
    let manager = PluginManager::new(
        data_root.to_path_buf(),
        env!("CARGO_PKG_VERSION").to_string(),
    );
    let report = if data_root.join("plugins").join(&manifest.id).exists() {
        let backup = data_root.join("plugins").join("backups");
        manager.update(&plugin_dir, &backup)?
    } else {
        manager.install(&plugin_dir)?
    };
    let _ = std::fs::remove_dir_all(&temp_root);
    Ok(json!({
        "ok": true,
        "report": report,
        "registry": registry.source,
    }))
}

/// 拉取远程 registry 并写入本地 market.json（refresh 语义）。
pub async fn refresh_local_market(
    url: Option<&str>,
    data_root: &Path,
) -> Result<FetchedRegistry, String> {
    let fetched = fetch_registry(url, data_root).await?;
    let path = data_root.join("plugins").join("market.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(
        path,
        serde_json::to_string_pretty(&fetched.manifest).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(fetched)
}
