use owo_agent_core::learn::{
    recorded_actions_from_sequence, FlowSkillStore, LearnPipeline, Sensitivity,
};
use owo_agent_core::memory::{Outcome, SemanticMemory};
use owo_agent_core::observe::{MemoryStore, Observation};
use owo_agent_core::skill_health::SkillState;
use std::path::Path;

fn observation(summary: &str) -> Observation {
    Observation {
        ts: chrono::Utc::now().to_rfc3339(),
        app_id: "qq".to_string(),
        kind: "action".to_string(),
        summary: summary.to_string(),
        detail: serde_json::json!({}),
        state_hash: 0,
    }
}

#[test]
fn memory_store_recall_survives_reopen() {
    let dir = std::env::temp_dir().join(format!("owo-mem-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("observations.jsonl");

    let mut store = MemoryStore::new(path.clone());
    store.append(observation("点击发送按钮")).unwrap();
    store.append(observation("输入消息内容")).unwrap();
    let hits = store.recall("发送按钮", 5);
    assert_eq!(hits.len(), 1);
    assert!(hits[0].summary.contains("发送"));

    let reloaded = MemoryStore::new(path.clone());
    assert_eq!(reloaded.recall("发送按钮", 5).len(), 1, "语义索引应持久化");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn memory_store_mark_outcome_persists() {
    let dir = std::env::temp_dir().join(format!("owo-mem-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("observations.jsonl");

    let mut store = MemoryStore::new(path.clone());
    let observed = observation("发送文件成功");
    let ts = observed.ts.clone();
    store.append(observed).unwrap();
    assert!(store
        .mark_outcome(&ts, "qq", "发送文件成功", Outcome::Success)
        .unwrap());

    let hits = store.recall("发送文件成功", 5);
    assert_eq!(hits[0].outcome, Outcome::Success);

    let reloaded = MemoryStore::new(path);
    let hits = reloaded.recall("发送文件成功", 5);
    assert_eq!(hits[0].outcome, Outcome::Success, "结果判定应持久化");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn memory_store_clear_clears_semantic_index() {
    let dir = std::env::temp_dir().join(format!("owo-mem-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("observations.jsonl");

    let mut store = MemoryStore::new(path.clone());
    store.append(observation("点击发送按钮")).unwrap();
    assert_eq!(store.recall("发送按钮", 5).len(), 1);
    store.clear().unwrap();
    assert!(store.recall("发送按钮", 5).is_empty());
    let reloaded = MemoryStore::new(path);
    assert!(reloaded.recall("发送按钮", 5).is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn semantic_memory_prune_drops_oldest_and_rebuilds_index() {
    let mut memory = SemanticMemory::new();
    for i in 0..5 {
        memory.add_observation(&observation(&format!("动作 {i}")));
    }
    memory.prune(3);
    assert_eq!(memory.len(), 3);
    assert!(
        memory
            .recall("动作 0", 5)
            .iter()
            .all(|entry| entry.summary != "动作 0"),
        "最旧条目应被淘汰"
    );
    assert!(memory
        .recall("动作 4", 5)
        .iter()
        .any(|entry| entry.summary == "动作 4"));
}

fn make_skill(root: &Path) -> FlowSkillStore {
    let pipeline = LearnPipeline::new(root.join("skills"));
    let samples = recorded_actions_from_sequence(
        "qq",
        &["click:发送".to_string(), "type:输入消息".to_string()],
    );
    pipeline
        .sink_from_actions(
            "send-file",
            vec!["qq".to_string()],
            Sensitivity::Low,
            "测试技能",
            samples,
        )
        .unwrap();
    pipeline.store
}

#[test]
fn flow_skill_health_gate_blocks_degraded_without_ack_and_recovers() {
    let dir = std::env::temp_dir().join(format!("owo-health-{}", uuid::Uuid::new_v4()));
    let store = make_skill(&dir);

    assert_eq!(store.health_state("send-file"), SkillState::Active);
    store.execution_gate("send-file", false).unwrap();

    store
        .record_execution("send-file", false, "send", "锚点未找到")
        .unwrap();
    let state = store
        .record_execution("send-file", false, "send", "锚点未找到")
        .unwrap();
    assert_eq!(state, SkillState::Degraded);

    let blocked = store.execution_gate("send-file", false);
    assert!(blocked.is_err());
    assert!(blocked.unwrap_err().contains("degraded_ack"));
    store.execution_gate("send-file", true).unwrap();

    store
        .record_execution("send-file", true, "send", "")
        .unwrap();
    assert_eq!(store.health_state("send-file"), SkillState::Active);
    store.execution_gate("send-file", false).unwrap();

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn flow_skill_health_persists_and_resets() {
    let dir = std::env::temp_dir().join(format!("owo-health-{}", uuid::Uuid::new_v4()));
    let store = make_skill(&dir);
    store
        .record_execution("send-file", false, "send", "失败")
        .unwrap();
    store
        .record_execution("send-file", false, "send", "失败")
        .unwrap();
    assert_eq!(store.health_state("send-file"), SkillState::Degraded);

    let reloaded = FlowSkillStore::new(dir.join("skills"));
    assert_eq!(
        reloaded.health_state("send-file"),
        SkillState::Degraded,
        "健康状态应持久化到 health.json"
    );
    reloaded.reset_health("send-file").unwrap();
    assert_eq!(reloaded.health_state("send-file"), SkillState::Active);

    let _ = std::fs::remove_dir_all(&dir);
}
