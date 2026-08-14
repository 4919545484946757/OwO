use owo_agent_core::audit::AuditEntry;
use owo_agent_core::session::SessionStore;
use owo_agent_core::sqlite_store::{AuditQuery, SqliteSessionStore};

fn entry(event: &str, tool: Option<&str>, detail: &str, approved: Option<bool>) -> AuditEntry {
    AuditEntry {
        ts: "2026-08-13T00:00:00Z".to_string(),
        session_id: "s1".to_string(),
        event: event.to_string(),
        tool: tool.map(String::from),
        approved,
        detail: detail.to_string(),
    }
}

fn seed(store: &SqliteSessionStore) {
    let entries = vec![
        entry(
            "tool_call",
            Some("read_file"),
            "读取 config.ini 文件 ok",
            Some(true),
        ),
        entry(
            "tool_call",
            Some("write_file"),
            "写入 main.rs ok",
            Some(true),
        ),
        entry("egress", None, "egress 请求", Some(false)),
        entry("settings", None, "settings 修改", Some(true)),
        entry(
            "tool_call",
            Some("read_file"),
            "读取 README 100%_完成",
            Some(true),
        ),
        entry("tool_call", Some("read_file"), "读取 secret", Some(false)),
    ];
    store.append_audit(&entries).unwrap();
}

#[test]
fn audit_pagination_returns_page_and_total() {
    let path = std::env::temp_dir().join(format!("owo-audit-page-{}.db", uuid::Uuid::new_v4()));
    let store = SqliteSessionStore::open(&path).unwrap();
    seed(&store);

    let (page, total) = store.query_audit(&AuditQuery {
        limit: 2,
        offset: 1,
        ..Default::default()
    });
    assert_eq!(total, 6);
    assert_eq!(page.len(), 2);
    // ORDER BY id DESC：id=5（README）与 id=4（settings 修改）。
    assert_eq!(page[0].detail, "读取 README 100%_完成");
    assert_eq!(page[1].detail, "settings 修改");

    let (all, total_all) = store.query_audit(&AuditQuery::default());
    assert_eq!(total_all, 6);
    assert_eq!(all.len(), 6);

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}

#[test]
fn audit_filters_combine_event_tool_and_approved() {
    let path = std::env::temp_dir().join(format!("owo-audit-filter-{}.db", uuid::Uuid::new_v4()));
    let store = SqliteSessionStore::open(&path).unwrap();
    seed(&store);

    let (_, total) = store.query_audit(&AuditQuery {
        event: Some("tool_call".to_string()),
        ..Default::default()
    });
    assert_eq!(total, 4);

    let (_, total) = store.query_audit(&AuditQuery {
        tool: Some("read_file".to_string()),
        ..Default::default()
    });
    assert_eq!(total, 3);

    let (_, total) = store.query_audit(&AuditQuery {
        approved: Some(false),
        ..Default::default()
    });
    assert_eq!(total, 2);

    let (_, total) = store.query_audit(&AuditQuery {
        event: Some("tool_call".to_string()),
        tool: Some("read_file".to_string()),
        approved: Some(true),
        ..Default::default()
    });
    assert_eq!(total, 2);

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}

#[test]
fn audit_search_matches_and_escapes_wildcards() {
    let path = std::env::temp_dir().join(format!("owo-audit-search-{}.db", uuid::Uuid::new_v4()));
    let store = SqliteSessionStore::open(&path).unwrap();
    seed(&store);

    let (hits, total) = store.query_audit(&AuditQuery {
        q: Some("config".to_string()),
        ..Default::default()
    });
    assert_eq!(total, 1);
    assert_eq!(hits[0].detail, "读取 config.ini 文件 ok");

    // 通配符应被转义：单独的 % 只按字面命中含 "%" 的记录（若未转义会命中全部）。
    let (hits, total) = store.query_audit(&AuditQuery {
        q: Some("%".to_string()),
        ..Default::default()
    });
    assert_eq!(total, 1);
    assert!(hits[0].detail.contains("100%"));

    let (_, total) = store.query_audit(&AuditQuery {
        q: Some("100%_完成".to_string()),
        ..Default::default()
    });
    assert_eq!(total, 1);

    let (_, total) = store.query_audit(&AuditQuery {
        q: Some("egress".to_string()),
        ..Default::default()
    });
    assert_eq!(total, 1);

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}
