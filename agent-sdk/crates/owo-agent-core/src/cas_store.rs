// R11:cas_store 质量收尾完成
//! 内容寻址存储：输入/输出/中间产物按哈希落盘、引用计数、引用计数清理。
//!
//! 设计来源：《多Agent并行体系-生产级设计与跨机扩展-2026-08-16.md》§2 产物管理：
//! - 内容按 SHA-256 寻址：相同内容只存一份；`put` 返回哈希引用。
//! - 引用计数：`ref_add`/`ref_release`；`gc` 清理引用归零的文件（不误删仍被引用的产物）。
//! - 崩溃恢复：引用表落盘（`refs.json`）；重启后按引用表重建计数，孤儿文件由 `gc` 回收。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// 引用表文件名（崩溃恢复时读取）。
const REFS_FILE: &str = "refs.json";

/// 内容寻址存储（Clone 共享同一目录与引用表）。
#[derive(Clone, Debug)]
pub struct CasStore {
    dir: PathBuf,
    inner: Arc<Mutex<CasInner>>,
}

#[derive(Default, Debug)]
struct CasInner {
    /// 哈希 → 引用计数。
    refs: HashMap<String, u64>,
}

/// 引用表快照（落盘格式）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CasRefsSnapshot {
    pub refs: HashMap<String, u64>,
}

impl CasStore {
    /// 新建存储（目录自动创建；存在引用表时恢复计数）。
    pub fn new(dir: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&dir).map_err(|e| format!("CAS 目录创建失败：{e}"))?;
        let store = Self {
            dir,
            inner: Arc::new(Mutex::new(CasInner::default())),
        };
        store.load_refs()?;
        Ok(store)
    }

    /// 内容哈希（SHA-256 十六进制）。
    pub fn hash_of(content: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content);
        format!("{:x}", hasher.finalize())
    }

    /// 写入内容（已存在则仅加引用），返回内容哈希。
    pub fn put(&self, content: &[u8]) -> Result<String, String> {
        let hash = Self::hash_of(content);
        let path = self.dir.join(&hash);
        if !path.exists() {
            let tmp = self.dir.join(format!("{hash}.tmp"));
            fs::write(&tmp, content).map_err(|e| format!("CAS 写入失败：{e}"))?;
            fs::rename(&tmp, &path).map_err(|e| format!("CAS 落盘失败：{e}"))?;
        }
        self.ref_add(&hash);
        Ok(hash)
    }

    /// 读取内容（无引用或不存在 → None）。
    pub fn get(&self, hash: &str) -> Option<Vec<u8>> {
        fs::read(self.dir.join(hash)).ok()
    }

    /// 按哈希读为字符串（诊断/冒烟方便）。
    pub fn get_text(&self, hash: &str) -> Option<String> {
        self.get(hash)
            .map(|b| String::from_utf8_lossy(&b).to_string())
    }

    pub fn contains(&self, hash: &str) -> bool {
        self.dir.join(hash).exists()
    }

    /// 增加引用（已存在引用记录则 +1；新内容引用从 1 起）。
    pub fn ref_add(&self, hash: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            *inner.refs.entry(hash.to_string()).or_insert(0) += 1;
        }
    }

    /// 释放引用（引用计数减少；归零后文件由 `gc` 清理）。
    pub fn ref_release(&self, hash: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            if let Some(count) = inner.refs.get_mut(hash) {
                *count = count.saturating_sub(1);
            }
        }
    }

    pub fn ref_count(&self, hash: &str) -> u64 {
        self.inner
            .lock()
            .map(|i| i.refs.get(hash).copied().unwrap_or(0))
            .unwrap_or(0)
    }

    /// 清理引用归零的内容文件 + 无引用记录的孤儿文件（put 写盘后 ref_add 前崩溃的残留）
    /// + `.tmp` 写入残留；并落盘引用表。返回清理的文件数。
    pub fn gc(&self) -> Result<usize, String> {
        let mut cleaned = 0usize;
        // 1) 引用归零的哈希：删除文件 + 移除表项。
        let to_remove: Vec<String> = {
            let inner = self.inner.lock().unwrap();
            inner
                .refs
                .iter()
                .filter(|(_, count)| **count == 0)
                .map(|(hash, _)| hash.clone())
                .collect()
        };
        for hash in to_remove {
            let path = self.dir.join(&hash);
            if path.exists() {
                fs::remove_file(&path).map_err(|e| format!("CAS 清理失败：{e}"))?;
                cleaned += 1;
            }
            if let Ok(mut inner) = self.inner.lock() {
                inner.refs.remove(&hash);
            }
        }
        // 2) 孤儿文件：目录内 hash 文件名（64 hex）但引用表中无记录 → 崩溃残留，删除。
        //    `.tmp` 写入残留 → 删除；`refs.json` 与目录跳过。
        let refs: std::collections::HashSet<String> = self
            .inner
            .lock()
            .map(|i| i.refs.keys().cloned().collect())
            .unwrap_or_default();
        let is_hash = |name: &str| name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit());
        let entries = fs::read_dir(&self.dir).map_err(|e| format!("CAS 目录扫描失败：{e}"))?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == REFS_FILE {
                continue;
            }
            if name.ends_with(".tmp") {
                fs::remove_file(entry.path()).map_err(|e| format!("CAS tmp 清理失败：{e}"))?;
                cleaned += 1;
                continue;
            }
            if is_hash(&name) && !refs.contains(&name) {
                fs::remove_file(entry.path()).map_err(|e| format!("CAS 孤儿清理失败：{e}"))?;
                cleaned += 1;
            }
        }
        self.save_refs()?;
        Ok(cleaned)
    }

    /// 持久化引用表（崩溃恢复依据）。
    pub fn save_refs(&self) -> Result<(), String> {
        let inner = self.inner.lock().unwrap();
        let snapshot = CasRefsSnapshot {
            refs: inner.refs.clone(),
        };
        let json = serde_json::to_string_pretty(&snapshot)
            .map_err(|e| format!("CAS 引用表序列化失败：{e}"))?;
        fs::write(self.dir.join(REFS_FILE), json).map_err(|e| format!("CAS 引用表写入失败：{e}"))
    }

    /// 崩溃恢复：读取引用表重建计数。
    pub fn load_refs(&self) -> Result<(), String> {
        let path = self.dir.join(REFS_FILE);
        if !path.exists() {
            return Ok(());
        }
        let json = fs::read_to_string(&path).map_err(|e| format!("CAS 引用表读取失败：{e}"))?;
        let snapshot: CasRefsSnapshot =
            serde_json::from_str(&json).map_err(|e| format!("CAS 引用表解析失败：{e}"))?;
        if let Ok(mut inner) = self.inner.lock() {
            inner.refs = snapshot.refs;
        }
        Ok(())
    }

    /// 目录内文件数（诊断）。
    pub fn file_count(&self) -> usize {
        fs::read_dir(&self.dir)
            .map(|entries| entries.filter_map(|e| e.ok()).count().saturating_sub(1))
            .unwrap_or(0)
    }
}
