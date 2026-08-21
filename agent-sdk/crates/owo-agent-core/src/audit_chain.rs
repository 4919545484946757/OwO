//! 审计防篡改链（综合文档 §6 P0 / X04，Wave 2：沙箱事件汇入 + 密钥凭据库托管）。
//!
//! - append-only 审计条目（无 UPDATE/DELETE 路径，`seq` 单调自增）；
//! - 分段 HMAC-SHA256 链：每条记录哈希 = HMAC(key, 前驱哈希 ‖ 记录规范字节)，
//!   每 `segment_len` 条锚定一次（`Anchor`），锚点可用于独立离线校验；
//! - `verify` 可检出任意篡改（改字段 / 删记录 / 重排 / 伪造插入 / 篡改锚点）；
//! - `append_sandbox_log`：沙箱审计事件（`SandboxAuditLog`）汇入审计链；
//! - `from_managed_key`：链密钥托管到凭据库（与导出文件分离存放）；
//! - `owo-agent audit verify|export` CLI 骨架（仅本模块，不接 main 主流程）。
//!
//! 哈希依赖 `sha2`（工作区既有依赖），HMAC 就地实现，**不引入新依赖**。

use crate::credentials::CredentialStore;
use crate::sandbox::SandboxAuditLog;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

/// 导出格式版本。
pub const AUDIT_CHAIN_VERSION: &str = "1";

/// HMAC 起源常量（派生 genesis 哈希）。
const GENESIS_LABEL: &[u8] = b"owo-audit-genesis-v1";

/// 托管密钥的凭据库条目默认名。
pub const AUDIT_KEY_STORE_KEY: &str = "owo-agent/audit-chain-key";

/// 审计链错误：校验失败必须显式报告位置与原因。
#[derive(Debug, thiserror::Error)]
pub enum AuditChainError {
    #[error("审计链校验失败（#{index}）：{reason}")]
    VerifyFailed { index: usize, reason: String },
    #[error("审计条目序号违反 append-only：期望 {expected}，实际 {actual}")]
    AppendOnlyViolation { expected: u64, actual: u64 },
    #[error("审计链格式错误：{0}")]
    Invalid(String),
    #[error("io 错误：{0}")]
    Io(#[from] std::io::Error),
    #[error("json 错误：{0}")]
    Json(#[from] serde_json::Error),
}

/// 审计记录（明文，可导出；`seq` 由链分配，单调递增）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditRecord {
    pub seq: u64,
    pub ts: String,
    pub actor: String,
    pub event: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
}

impl AuditRecord {
    pub fn new(actor: &str, event: &str, detail: impl Into<String>) -> Self {
        Self {
            seq: 0,
            ts: chrono::Utc::now().to_rfc3339(),
            actor: actor.to_string(),
            event: event.to_string(),
            detail: detail.into(),
            tool: None,
        }
    }

    pub fn with_tool(mut self, tool: &str) -> Self {
        self.tool = Some(tool.to_string());
        self
    }
}

/// 记录规范字节（hash 的输入之一；字段序固定，保证跨进程一致）。
pub fn canonical(record: &AuditRecord) -> Vec<u8> {
    serde_json::to_vec(record).expect("AuditRecord 序列化不应失败")
}

/// HMAC-SHA256（就地实现，避免新增依赖）。
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK_SIZE: usize = 64;
    let mut key_pad = [0u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        let digest = Sha256::digest(key);
        key_pad[..32].copy_from_slice(&digest);
    } else {
        key_pad[..key.len()].copy_from_slice(key);
    }
    let mut inner = [0u8; BLOCK_SIZE];
    let mut outer = [0u8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        inner[i] = key_pad[i] ^ 0x36;
        outer[i] = key_pad[i] ^ 0x5c;
    }
    let inner_hash = {
        let mut hasher = Sha256::new();
        hasher.update(inner);
        hasher.update(data);
        hasher.finalize()
    };
    let mut hasher = Sha256::new();
    hasher.update(outer);
    hasher.update(inner_hash);
    hasher.finalize().into()
}

/// 十六进制编码。
pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{:02x}", byte)).collect()
}

/// 链上记录：记录 + 前驱哈希 + 自身哈希。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChainedRecord {
    pub record: AuditRecord,
    pub prev_hash: String,
    pub hash: String,
}

/// 分段锚点：锚定 `seq` 条记录的链哈希。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Anchor {
    pub seq: u64,
    pub hash: String,
}

/// 可离线导出/校验的审计链快照。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditExport {
    pub version: String,
    pub segment_len: usize,
    pub records: Vec<ChainedRecord>,
    pub anchors: Vec<Anchor>,
}

/// append-only 审计链。
#[derive(Debug, Clone)]
pub struct AuditChain {
    records: Vec<ChainedRecord>,
    anchors: Vec<Anchor>,
    segment_len: usize,
    key: Vec<u8>,
}

impl AuditChain {
    /// 新建空链。`segment_len = 0` 按 1 处理（每条都锚定，退化为全锚定）。
    pub fn new(key: &[u8], segment_len: usize) -> Self {
        Self {
            records: Vec::new(),
            anchors: Vec::new(),
            segment_len: segment_len.max(1),
            key: key.to_vec(),
        }
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn segment_len(&self) -> usize {
        self.segment_len
    }

    /// append-only：`seq` 由链分配（= 当前长度），外部传入的 `seq` 被覆盖。
    /// 返回分配的 seq。
    pub fn append(&mut self, mut record: AuditRecord) -> u64 {
        let seq = self.records.len() as u64;
        record.seq = seq;
        let prev = self
            .records
            .last()
            .map(|record| record.hash.clone())
            .unwrap_or_else(|| genesis_hash(&self.key));
        let hash = hex_encode(&hmac_sha256(
            &self.key,
            &[prev.as_bytes(), &canonical(&record)].concat(),
        ));
        let chained = ChainedRecord {
            record,
            prev_hash: prev,
            hash: hash.clone(),
        };
        if (seq + 1).is_multiple_of(self.segment_len as u64) {
            self.anchors.push(Anchor { seq, hash });
        }
        self.records.push(chained);
        seq
    }

    pub fn records(&self) -> &[ChainedRecord] {
        &self.records
    }

    pub fn anchors(&self) -> &[Anchor] {
        &self.anchors
    }

    /// 全链校验：hash 重放 + 锚点一致性 + append-only 序号约束。
    pub fn verify(&self) -> Result<(), AuditChainError> {
        verify_parts(&self.records, &self.anchors, &self.key, self.segment_len)
    }

    /// 沙箱审计事件汇入审计链（append-only）。
    pub fn append_sandbox_log(&mut self, log: &SandboxAuditLog, actor: &str) -> usize {
        let count = log.len();
        for event in log.events() {
            let mut record = AuditRecord::new(
                actor,
                &format!("sandbox.{}", event.kind.label()),
                event.detail.clone(),
            );
            record.ts = event.ts.clone();
            record.tool = Some(format!("sandbox:{}", event.sandbox));
            self.append(record);
        }
        count
    }

    /// egress 拒绝事件汇入审计链（R9：越界网络拒绝可追溯）。
    pub fn append_egress_rejection(&mut self, actor: &str, target: &str, reason: &str) -> u64 {
        self.append(
            AuditRecord::new(
                actor,
                "egress.rejected",
                format!("target={target}；{reason}"),
            )
            .with_tool("egress"),
        )
    }

    /// 从凭据库托管密钥构造审计链：无密钥则生成 32 字节随机密钥（hex 编码入库）。
    /// 凭据库不可用 → 显式错误（禁止静默使用未托管密钥）。
    pub fn from_managed_key(
        store: &dyn CredentialStore,
        store_key: &str,
        segment_len: usize,
    ) -> Result<Self, AuditChainError> {
        if !store.available() {
            return Err(AuditChainError::Invalid(
                "审计链密钥托管凭据库不可用（拒绝无托管密钥的静默回退）".to_string(),
            ));
        }
        let key = match store.get(store_key) {
            Some(stored) => decode_hex(&stored)
                .map_err(|error| AuditChainError::Invalid(format!("托管密钥损坏：{error}")))?,
            None => {
                let generated: Vec<u8> = uuid::Uuid::new_v4()
                    .as_bytes()
                    .iter()
                    .chain(uuid::Uuid::new_v4().as_bytes().iter())
                    .copied()
                    .collect();
                store
                    .set(store_key, &hex_encode(&generated))
                    .map_err(|error| {
                        AuditChainError::Invalid(format!("托管密钥写入失败：{error}"))
                    })?;
                generated
            }
        };
        Ok(Self::new(&key, segment_len))
    }

    /// 密钥摘要（不泄露密钥本身，供"导出与密钥分离"契约验证）。
    pub fn key_digest(&self) -> String {
        hex_encode(&hmac_sha256(&self.key, b"audit-key-digest"))
    }

    /// 强制轮换托管密钥：生成新 32 字节密钥并经凭据库轮换（覆盖 + 读回校验）。
    /// 返回新链（旧链导出的记录在新密钥下无法验证——属预期语义）。
    pub fn force_rotate_managed_key(
        store: &dyn CredentialStore,
        store_key: &str,
        segment_len: usize,
    ) -> Result<Self, AuditChainError> {
        if !store.available() {
            return Err(AuditChainError::Invalid(
                "审计链密钥托管凭据库不可用（拒绝无托管密钥的静默回退）".to_string(),
            ));
        }
        let generated: Vec<u8> = uuid::Uuid::new_v4()
            .as_bytes()
            .iter()
            .chain(uuid::Uuid::new_v4().as_bytes().iter())
            .copied()
            .collect();
        let hex_key = hex_encode(&generated);
        store
            .rotate(store_key, &hex_key)
            .map_err(|error| AuditChainError::Invalid(format!("托管密钥轮换失败：{error}")))?;
        Ok(Self::new(&generated, segment_len))
    }

    /// 导出快照（含链与锚点）。
    pub fn export(&self) -> AuditExport {
        AuditExport {
            version: AUDIT_CHAIN_VERSION.to_string(),
            segment_len: self.segment_len,
            records: self.records.clone(),
            anchors: self.anchors.clone(),
        }
    }
}

fn genesis_hash(key: &[u8]) -> String {
    hex_encode(&hmac_sha256(key, GENESIS_LABEL))
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// 十六进制解码（托管密钥往返）。
fn decode_hex(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) {
        return Err("密钥长度不是合法十六进制".to_string());
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    for chunk in text.as_bytes().chunks(2) {
        let high = hex_digit(chunk[0]).ok_or_else(|| "密钥含非十六进制字符".to_string())?;
        let low = hex_digit(chunk[1]).ok_or_else(|| "密钥含非十六进制字符".to_string())?;
        out.push((high << 4) | low);
    }
    Ok(out)
}

fn verify_parts(
    records: &[ChainedRecord],
    anchors: &[Anchor],
    key: &[u8],
    segment_len: usize,
) -> Result<(), AuditChainError> {
    let genesis = genesis_hash(key);
    let mut prev = genesis;
    for (index, chained) in records.iter().enumerate() {
        let seq = index as u64;
        if chained.record.seq != seq {
            return Err(AuditChainError::AppendOnlyViolation {
                expected: seq,
                actual: chained.record.seq,
            });
        }
        if chained.prev_hash != prev {
            return Err(AuditChainError::VerifyFailed {
                index,
                reason: "前驱哈希断裂（记录被删除或重排）".to_string(),
            });
        }
        let expected = hex_encode(&hmac_sha256(
            key,
            &[prev.as_bytes(), &canonical(&chained.record)].concat(),
        ));
        if chained.hash != expected {
            return Err(AuditChainError::VerifyFailed {
                index,
                reason: "记录哈希不匹配（内容被篡改）".to_string(),
            });
        }
        prev = chained.hash.clone();
    }

    let segment_len = segment_len.max(1) as u64;
    // 锚点必须严格递增（无重复、有序）。
    for pair in anchors.windows(2) {
        if pair[0].seq >= pair[1].seq {
            return Err(AuditChainError::VerifyFailed {
                index: pair[1].seq as usize,
                reason: "锚点序号未严格递增".to_string(),
            });
        }
    }
    // 每个分段边界必须有且仅有一个匹配锚点。
    let mut cursor = 0usize;
    for (index, chained) in records.iter().enumerate() {
        let seq = index as u64;
        if (seq + 1).is_multiple_of(segment_len) {
            let anchor = anchors
                .get(cursor)
                .ok_or_else(|| AuditChainError::VerifyFailed {
                    index,
                    reason: "分段边界缺少锚点".to_string(),
                })?;
            if anchor.seq != seq {
                return Err(AuditChainError::VerifyFailed {
                    index,
                    reason: format!("锚点序号 {} 不落在分段边界", anchor.seq),
                });
            }
            if anchor.hash != chained.hash {
                return Err(AuditChainError::VerifyFailed {
                    index,
                    reason: format!("锚点哈希不匹配（seq={} 段被整体篡改）", anchor.seq),
                });
            }
            cursor += 1;
        }
    }
    if cursor < anchors.len() {
        return Err(AuditChainError::VerifyFailed {
            index: records.len(),
            reason: format!(
                "存在未消费的锚点（seq={}，超出记录范围）",
                anchors[cursor].seq
            ),
        });
    }
    Ok(())
}

/// 校验导出的快照（离线验证入口）。
pub fn verify_export(export: &AuditExport, key: &[u8]) -> Result<(), AuditChainError> {
    if export.version != AUDIT_CHAIN_VERSION {
        return Err(AuditChainError::Invalid(format!(
            "不支持导出版本 {}（当前 {})",
            export.version, AUDIT_CHAIN_VERSION
        )));
    }
    verify_parts(&export.records, &export.anchors, key, export.segment_len)
}

/// 导出到文件（JSON）。
pub fn export_to_file(export: &AuditExport, path: &Path) -> Result<(), AuditChainError> {
    let json = serde_json::to_string_pretty(export)?;
    std::fs::write(path, json)?;
    Ok(())
}

/// 加密导出到文件（R10：审计导出信封加密，密钥与导出文件分离）。
pub fn export_encrypted_to_file(
    export: &AuditExport,
    path: &Path,
    dek: &[u8; 32],
) -> Result<(), AuditChainError> {
    let json = serde_json::to_vec(export)?;
    crate::storage_crypto::encrypt_file_envelope_with_dek(path, &json, dek)
        .map_err(|error| AuditChainError::Invalid(format!("审计导出加密失败：{error}")))?;
    Ok(())
}

/// 从加密文件加载导出（R10：校验信封格式 + 显式解密错误）。
pub fn load_encrypted_export(path: &Path, dek: &[u8; 32]) -> Result<AuditExport, AuditChainError> {
    let plain = crate::storage_crypto::decrypt_file_envelope_with_dek(path, dek)
        .map_err(|error| AuditChainError::Invalid(format!("审计导出解密失败：{error}")))?;
    let export: AuditExport = serde_json::from_slice(&plain)?;
    Ok(export)
}

/// 从文件加载导出。
pub fn load_export(path: &Path) -> Result<AuditExport, AuditChainError> {
    let content = std::fs::read_to_string(path)?;
    let export: AuditExport = serde_json::from_str(&content)?;
    Ok(export)
}

/// 校验导出文件（`owo-agent audit verify` 骨架）。
pub fn verify_file(path: &Path, key: &[u8]) -> Result<(), AuditChainError> {
    let export = load_export(path)?;
    verify_export(&export, key)
}

/// CLI 命令骨架（`owo-agent audit verify|export`，仅本模块；不接 main）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditCliCommand {
    Verify { path: String },
    Export { path: String, out: String },
}

/// CLI 执行结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditCliOutcome {
    VerifyOk { records: usize, anchors: usize },
    Exported { out: String, records: usize },
}

/// 执行审计 CLI 骨架命令。
pub fn run_audit_cli(
    command: &AuditCliCommand,
    key: &[u8],
) -> Result<AuditCliOutcome, AuditChainError> {
    match command {
        AuditCliCommand::Verify { path } => {
            let export = load_export(Path::new(path))?;
            verify_export(&export, key)?;
            Ok(AuditCliOutcome::VerifyOk {
                records: export.records.len(),
                anchors: export.anchors.len(),
            })
        }
        AuditCliCommand::Export { path, out } => {
            let export = load_export(Path::new(path))?;
            export_to_file(&export, Path::new(out))?;
            Ok(AuditCliOutcome::Exported {
                out: out.clone(),
                records: export.records.len(),
            })
        }
    }
}
