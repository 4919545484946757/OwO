//! audit_chain.rs 契约测试（X04）：append-only 序号、分段 HMAC 链、
//! 任意篡改检出（改字段/删记录/重排/伪造/锚点篡改）、导出与离线校验、CLI 骨架
//! （Wave 1）+ 沙箱事件汇入与密钥凭据库托管（Wave 2）。

use owo_agent_core::audit_chain::*;
use owo_agent_core::credentials::{CredentialStore, MemoryCredentialStore};
use owo_agent_core::sandbox::{SandboxAuditLog, SandboxEventKind};

const KEY: &[u8] = b"wave1-test-key";

fn record(actor: &str, event: &str, detail: &str) -> AuditRecord {
    AuditRecord::new(actor, event, detail)
}

fn chained_without_links(record: AuditRecord) -> ChainedRecord {
    ChainedRecord {
        record,
        prev_hash: String::new(),
        hash: String::new(),
    }
}

#[test]
fn hmac_sha256_matches_known_vector() {
    // RFC 4231 Test Case 1：key=0x0b×20，data="Hi There"。
    let digest = hmac_sha256(&[0x0b; 20], b"Hi There");
    assert_eq!(
        hex_encode(&digest),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
}

#[test]
fn empty_chain_verifies() {
    let chain = AuditChain::new(KEY, 10);
    assert!(chain.is_empty());
    assert!(chain.verify().is_ok());
}

#[test]
fn append_and_verify_ok() {
    let mut chain = AuditChain::new(KEY, 100);
    chain.append(record("agent", "tool_call", "读取 main.rs"));
    chain.append(record("main", "approve", "批准写入").with_tool("write_file"));
    chain.append(record("plugin", "spawn", "插件启动"));
    assert_eq!(chain.len(), 3);
    assert!(chain.verify().is_ok());
}

#[test]
fn seq_is_append_only_monotonic() {
    let mut chain = AuditChain::new(KEY, 100);
    let first = chain.append(record("agent", "a", "1"));
    let second = chain.append(record("agent", "b", "2"));
    assert_eq!(first, 0);
    assert_eq!(second, 1);
    // 外部传入的 seq 被链覆盖（不可伪造）。
    let mut forged = record("agent", "c", "3");
    forged.seq = 999;
    assert_eq!(chain.append(forged), 2);
    assert!(chain.verify().is_ok());
}

#[test]
fn tamper_detail_detected() {
    let mut chain = AuditChain::new(KEY, 100);
    chain.append(record("agent", "tool_call", "读取 main.rs"));
    chain.append(record("agent", "tool_call", "写入 config"));
    let mut export = chain.export();
    export.records[1].record.detail = "读取 secret".to_string();
    let err = verify_export(&export, KEY).unwrap_err();
    assert!(matches!(
        err,
        AuditChainError::VerifyFailed { index: 1, .. }
    ));
}

#[test]
fn tamper_actor_detected() {
    let mut chain = AuditChain::new(KEY, 100);
    chain.append(record("agent", "approve", "ok"));
    let mut export = chain.export();
    export.records[0].record.actor = "attacker".to_string();
    assert!(verify_export(&export, KEY).is_err());
}

#[test]
fn tamper_tool_detected() {
    let mut chain = AuditChain::new(KEY, 100);
    chain.append(record("agent", "tool_call", "ok").with_tool("read_file"));
    let mut export = chain.export();
    export.records[0].record.tool = None;
    assert!(verify_export(&export, KEY).is_err());
}

#[test]
fn deleted_record_detected_as_link_break() {
    let mut chain = AuditChain::new(KEY, 100);
    chain.append(record("agent", "a", "1"));
    chain.append(record("agent", "b", "2"));
    chain.append(record("agent", "c", "3"));
    let mut export = chain.export();
    export.records.remove(1);
    // 删除 seq=1：先触发 append-only 序号跳变，重链后则触发前驱哈希断裂。
    let err = verify_export(&export, KEY).unwrap_err();
    assert!(matches!(
        err,
        AuditChainError::VerifyFailed { .. } | AuditChainError::AppendOnlyViolation { .. }
    ));
}

#[test]
fn seq_gap_detected_as_append_only_violation() {
    let mut export = AuditChain::new(KEY, 100).export();
    export.records.push(chained_without_links(AuditRecord {
        seq: 2,
        ..record("agent", "b", "2")
    }));
    let err = verify_export(&export, KEY).unwrap_err();
    assert!(matches!(err, AuditChainError::AppendOnlyViolation { .. }));
}

#[test]
fn reordered_records_detected() {
    let mut chain = AuditChain::new(KEY, 100);
    chain.append(record("agent", "a", "1"));
    chain.append(record("agent", "b", "2"));
    let mut export = chain.export();
    export.records.swap(0, 1);
    assert!(verify_export(&export, KEY).is_err());
}

#[test]
fn forged_insert_detected() {
    let mut chain = AuditChain::new(KEY, 100);
    chain.append(record("agent", "a", "1"));
    let mut export = chain.export();
    export.records.insert(
        1,
        chained_without_links(record("attacker", "approve", "伪造批准")),
    );
    assert!(verify_export(&export, KEY).is_err());
}

#[test]
fn anchors_created_at_segment_boundaries() {
    let mut chain = AuditChain::new(KEY, 2);
    chain.append(record("agent", "a", "1"));
    assert!(chain.anchors().is_empty());
    chain.append(record("agent", "b", "2"));
    assert_eq!(chain.anchors().len(), 1);
    assert_eq!(chain.anchors()[0].seq, 1);
    chain.append(record("agent", "c", "3"));
    chain.append(record("agent", "d", "4"));
    assert_eq!(chain.anchors().len(), 2);
    assert_eq!(chain.anchors()[1].seq, 3);
    assert!(chain.verify().is_ok());
}

#[test]
fn tampered_anchor_detected() {
    let mut chain = AuditChain::new(KEY, 2);
    for i in 0..4 {
        chain.append(record("agent", "e", &format!("事件 {}", i)));
    }
    let mut export = chain.export();
    export.anchors[0].hash = "ff".repeat(32);
    assert!(verify_export(&export, KEY).is_err());
}

#[test]
fn missing_anchor_detected() {
    let mut chain = AuditChain::new(KEY, 2);
    for i in 0..4 {
        chain.append(record("agent", "e", &format!("事件 {}", i)));
    }
    let mut export = chain.export();
    export.anchors.remove(0);
    assert!(verify_export(&export, KEY).is_err());
}

#[test]
fn relinked_tamper_still_caught_by_anchor() {
    // 攻击者持有 key，篡改内容并重算后续所有哈希：分段锚点仍能检出。
    let mut chain = AuditChain::new(KEY, 2);
    for i in 0..4 {
        chain.append(record("agent", "e", &format!("事件 {}", i)));
    }
    let mut records = chain.records().to_vec();
    records[1].record.detail = "被篡改".to_string();
    let mut prev = records[0].hash.clone();
    for chained in records.iter_mut().skip(1) {
        chained.prev_hash = prev.clone();
        chained.hash = hex_encode(&hmac_sha256(
            KEY,
            &[prev.as_bytes(), &canonical(&chained.record)].concat(),
        ));
        prev = chained.hash.clone();
    }
    // 重链后自身一致，但 seq=1 锚点仍指向旧哈希 → 检出。
    let export = AuditExport {
        version: AUDIT_CHAIN_VERSION.to_string(),
        segment_len: 2,
        records,
        anchors: chain.anchors().to_vec(),
    };
    assert!(verify_export(&export, KEY).is_err());
}

#[test]
fn export_round_trip_verifies() {
    let mut chain = AuditChain::new(KEY, 3);
    chain.append(record("agent", "a", "1"));
    chain.append(record("agent", "b", "2"));
    let export = chain.export();
    assert_eq!(export.version, AUDIT_CHAIN_VERSION);
    assert!(verify_export(&export, KEY).is_ok());
}

#[test]
fn unsupported_version_rejected() {
    let mut chain = AuditChain::new(KEY, 10);
    chain.append(record("agent", "a", "1"));
    let mut export = chain.export();
    export.version = "0".to_string();
    let err = verify_export(&export, KEY).unwrap_err();
    assert!(matches!(err, AuditChainError::Invalid(_)));
}

#[test]
fn export_file_and_verify_file() {
    let dir = std::env::temp_dir().join(format!("owo-audit-chain-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut chain = AuditChain::new(KEY, 2);
    chain.append(record("agent", "a", "1"));
    chain.append(record("agent", "b", "2"));
    chain.append(record("agent", "c", "3"));
    let path = dir.join("audit.json");
    export_to_file(&chain.export(), &path).unwrap();
    assert!(verify_file(&path, KEY).is_ok());

    // 篡改文件后离线校验必须检出。
    let content = std::fs::read_to_string(&path).unwrap();
    let tampered = content
        .replace("事件 1", "事件 100")
        .replace("\"1\"", "\"99\"");
    std::fs::write(&path, tampered).unwrap();
    assert!(verify_file(&path, KEY).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cli_export_then_verify() {
    let dir = std::env::temp_dir().join(format!("owo-audit-cli-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut chain = AuditChain::new(KEY, 2);
    chain.append(record("agent", "a", "1"));
    chain.append(record("agent", "b", "2"));
    let source = dir.join("src.json");
    let target = dir.join("out.json");
    export_to_file(&chain.export(), &source).unwrap();

    let exported = run_audit_cli(
        &AuditCliCommand::Export {
            path: source.to_string_lossy().into_owned(),
            out: target.to_string_lossy().into_owned(),
        },
        KEY,
    )
    .unwrap();
    assert!(matches!(
        exported,
        AuditCliOutcome::Exported { records: 2, .. }
    ));

    let verified = run_audit_cli(
        &AuditCliCommand::Verify {
            path: target.to_string_lossy().into_owned(),
        },
        KEY,
    )
    .unwrap();
    assert!(matches!(
        verified,
        AuditCliOutcome::VerifyOk {
            records: 2,
            anchors: 1
        }
    ));

    // 错误 key 校验必须显式失败（不 panic）。
    assert!(run_audit_cli(
        &AuditCliCommand::Verify {
            path: target.to_string_lossy().into_owned(),
        },
        b"wrong-key",
    )
    .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wrong_key_never_verifies() {
    let mut chain = AuditChain::new(b"key-a", 10);
    chain.append(record("agent", "a", "1"));
    assert!(verify_export(&chain.export(), b"key-b").is_err());
}

fn sandbox_log_with_events() -> SandboxAuditLog {
    let mut log = SandboxAuditLog::default();
    log.record(
        SandboxEventKind::SpawnRejected,
        "run_command",
        "命中危险黑名单",
    );
    log.record(
        SandboxEventKind::DegradedIsolation,
        "mcp:files",
        "显式降级到 JobOnly",
    );
    log.record(SandboxEventKind::Killed, "win-1234", "沙箱进程已终止");
    log
}

#[test]
fn sandbox_log_ingested_into_chain_and_verifies() {
    let mut chain = AuditChain::new(KEY, 100);
    let log = sandbox_log_with_events();
    let count = chain.append_sandbox_log(&log, "sandbox-agent");
    assert_eq!(count, 3);
    assert_eq!(chain.len(), 3);
    assert!(chain.verify().is_ok());
    // 事件名带 sandbox. 前缀。
    assert_eq!(chain.records()[0].record.event, "sandbox.spawn_rejected");
    assert_eq!(
        chain.records()[1].record.event,
        "sandbox.degraded_isolation"
    );
    assert_eq!(chain.records()[2].record.event, "sandbox.killed");
    assert_eq!(chain.records()[0].record.actor, "sandbox-agent");
}

#[test]
fn tampered_sandbox_event_detected_in_chain() {
    let mut chain = AuditChain::new(KEY, 100);
    let log = sandbox_log_with_events();
    chain.append_sandbox_log(&log, "sandbox-agent");
    let mut export = chain.export();
    export.records[1].record.detail = "被篡改的降级原因".to_string();
    assert!(verify_export(&export, KEY).is_err());
}

#[test]
fn managed_key_generates_and_stores_in_store() {
    let store = MemoryCredentialStore::new();
    let key_name = format!("audit-key-{}", uuid::Uuid::new_v4());
    let chain = AuditChain::from_managed_key(&store, &key_name, 10).unwrap();
    // 密钥已入库（hex 编码）。
    let stored = store.get(&key_name).unwrap();
    assert_eq!(stored.len(), 64);
    // 密钥摘要可验证（不泄露密钥本身）。
    let digest = chain.key_digest();
    assert_eq!(digest.len(), 64);
    assert!(!stored.contains(&digest));
    // 导出文件不含密钥。
    let export_json = serde_json::to_string(&chain.export()).unwrap();
    assert!(scan_secrets(&export_json, &[stored.as_str(), digest.as_str()]).is_empty());
}

#[test]
fn managed_key_reuses_existing_and_verifies() {
    let store = MemoryCredentialStore::new();
    let key_name = format!("audit-key-{}", uuid::Uuid::new_v4());
    let mut first = AuditChain::from_managed_key(&store, &key_name, 10).unwrap();
    first.append(record("agent", "a", "1"));
    // 第二次构造复用同一密钥 → 同一摘要 → 校验通过。
    let second = AuditChain::from_managed_key(&store, &key_name, 10).unwrap();
    assert_eq!(first.key_digest(), second.key_digest());
    assert!(verify_export(&first.export(), &decode_key(&store.get(&key_name).unwrap())).is_ok());
}

#[test]
fn managed_key_unavailable_store_is_explicit_error() {
    let store = MemoryCredentialStore::with_available(false);
    let err = AuditChain::from_managed_key(&store, "audit-key", 10).unwrap_err();
    assert!(matches!(err, AuditChainError::Invalid(_)));
}

#[test]
fn managed_key_corrupted_store_value_rejected() {
    let store = MemoryCredentialStore::new();
    let key_name = format!("audit-key-{}", uuid::Uuid::new_v4());
    store.set(&key_name, "not-a-valid-hex!").unwrap();
    let err = AuditChain::from_managed_key(&store, &key_name, 10).unwrap_err();
    assert!(matches!(err, AuditChainError::Invalid(_)));
}

fn decode_key(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16).unwrap() as u8;
            let low = (pair[1] as char).to_digit(16).unwrap() as u8;
            (high << 4) | low
        })
        .collect()
}

fn scan_secrets(json: &str, secrets: &[&str]) -> Vec<String> {
    secrets
        .iter()
        .filter(|secret| json.contains(**secret))
        .map(|secret| (*secret).to_string())
        .collect()
}
