//! 生产就绪冒烟集成测试（Agent 4）。
//!
//! 覆盖可操作生产就绪验证要求中的核心可靠性语义：
//! - 优雅关闭：`/server/shutdown` 二次确认 + 关闭后拒绝新回合；pid 文件生命周期与强杀恢复；
//! - SSE + Last-Event-ID：断线后按 `last_event_id` 续传零丢失；SSE 端点形态；
//! - Idempotency-Key：重复提交不产生重复副作用（注册表 executor 至多一次；`/fleet/tasks/submit` 同键同体 409）；
//! - 存储 /storage/backup|export|clear：确认语义与完整性结果。
//!
//! 约束：不使用真实模型 / 外部网络 / 长时间 sleep；走 tower oneshot 内存往返，
//! `event_stream` / `idempotency` 以 `#[path]` 独立编译（与既有测试同款模式）。

use owo_agent_core::permissions::Policy;
use owo_agent_core::sqlite_store::SqliteSessionStore;
use owo_agent_core::tools::ToolRegistry;
use owo_agent_core::Agent;
use owo_agent_server::build_router;
use owo_agent_server::shutdown::{ForceKillRecovery, PidFile, DEFAULT_MAX_CONCURRENT_TURNS};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

#[path = "../src/event_stream.rs"]
mod event_stream;

#[path = "../src/idempotency.rs"]
mod idempotency;

struct IdleProvider;

#[async_trait::async_trait]
impl owo_agent_core::gateway::ModelProvider for IdleProvider {
    async fn complete(
        &self,
        _messages: &[owo_agent_core::ChatMessage],
        _tools: &[owo_agent_core::ToolSpec],
    ) -> Result<owo_agent_core::ModelOutput, String> {
        Err("IdleProvider 不应被调用".to_string())
    }
}

async fn test_state() -> (Arc<owo_agent_server::AppState>, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let agent = Agent::new(
        Arc::new(IdleProvider),
        ToolRegistry::new(),
        Policy::new(&workspace),
        Default::default(),
    );
    // 与真实 serve 布局一致：index.db 位于数据根目录（temp.path()）下。
    let store = SqliteSessionStore::open(&temp.path().join("index.db")).unwrap();
    let state = Arc::new(owo_agent_server::AppState::new(
        agent,
        store,
        workspace.join("traces"),
        temp.path().to_path_buf(),
        workspace,
    ));
    (state, temp)
}

fn request(
    state: &Arc<owo_agent_server::AppState>,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> axum::http::Request<axum::body::Body> {
    request_with_headers(state, method, path, body, &[])
}

fn request_with_headers(
    state: &Arc<owo_agent_server::AppState>,
    method: &str,
    path: &str,
    body: Option<&str>,
    headers: &[(&str, &str)],
) -> axum::http::Request<axum::body::Body> {
    use axum::http::{header, Method, Request};
    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(path)
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", state.auth_token.token()),
        );
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    if let Some(body) = body {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
        return builder
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
    }
    builder.body(axum::body::Body::empty()).unwrap()
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

// ---------- R1 健康与 server status ----------

#[tokio::test]
async fn health_and_server_status_report_gate_and_storage() {
    let (state, _temp) = test_state().await;
    let app = build_router(Arc::clone(&state));

    let health = app
        .clone()
        .oneshot(request(&state, "GET", "/health", None))
        .await
        .unwrap();
    assert_eq!(health.status().as_u16(), 200, "/health 应 200");
    let health_json = body_json(health).await;
    assert_eq!(health_json["healthy"], true, "/health 应回 healthy");

    let status = app
        .clone()
        .oneshot(request(&state, "GET", "/server/status", None))
        .await
        .unwrap();
    assert_eq!(status.status().as_u16(), 200, "/server/status 应 200");
    let json = body_json(status).await;
    assert_eq!(
        json["shutdown_gate"]["max_concurrent_turns"], DEFAULT_MAX_CONCURRENT_TURNS as u64,
        "默认并发上限应上报"
    );
    assert_eq!(json["shutdown_gate"]["active_turns"], 0);
    assert_eq!(json["shutdown_gate"]["shutting_down"], false);
    assert_eq!(json["storage"]["read_only"], false);
}

// ---------- R5 优雅关闭 + pid 清理 ----------

#[tokio::test]
async fn pid_file_uses_drop_cleanup_and_recover_force_kill() {
    let temp = tempfile::tempdir().unwrap();

    // 正常生命周期：create → drop 自动清理。
    {
        let pid = PidFile::create(temp.path()).unwrap();
        assert!(temp.path().join("server.pid").is_file());
    }
    assert!(
        !temp.path().join("server.pid").exists(),
        "Drop 后 pid 文件应被清理"
    );

    // 强杀残留：陈旧 pid 被清理并报告。
    std::fs::write(temp.path().join("server.pid"), "999999999").unwrap();
    let recovery = owo_agent_server::shutdown::recover_force_kill(temp.path()).unwrap();
    assert_eq!(
        recovery,
        Some(ForceKillRecovery {
            stale_pid: Some(999_999_999),
            cleaned: true,
        })
    );
    assert!(
        !temp.path().join("server.pid").exists(),
        "陈旧 pid 应被清理"
    );
}

#[tokio::test]
async fn server_shutdown_requires_confirm_and_blocks_new_turns() {
    let (state, _temp) = test_state().await;
    let app = build_router(Arc::clone(&state));

    // 无确认 → 400。
    let no_confirm = app
        .clone()
        .oneshot(request(&state, "POST", "/server/shutdown", Some("{}")))
        .await
        .unwrap();
    assert_eq!(no_confirm.status().as_u16(), 400, "缺少二次确认应 400");

    // 确认 → 200 且停止接收。
    let confirmed = app
        .clone()
        .oneshot(request(
            &state,
            "POST",
            "/server/shutdown",
            Some(r#"{"confirm":true}"#),
        ))
        .await
        .unwrap();
    assert_eq!(confirmed.status().as_u16(), 200);
    assert!(
        state.shutdown_gate.shutting_down(),
        "shutdown_gate 应进入关闭态"
    );

    // 关闭中新回合 → 503（会话创建不受 turn 门控影响，仅回合被拒）。
    let created = app
        .clone()
        .oneshot(request(
            &state,
            "POST",
            "/session",
            Some(r#"{"workspace":".","model":"idle"}"#),
        ))
        .await
        .unwrap();
    let session_id = {
        let bytes = axum::body::to_bytes(created.into_body(), 1024 * 1024)
            .await
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&bytes)
            .unwrap()
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap()
            .to_string()
    };
    let turn = app
        .clone()
        .oneshot(request(
            &state,
            "POST",
            &format!("/session/{session_id}/turn"),
            Some(r#"{"prompt":"hi"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(turn.status().as_u16(), 503, "关闭中拒绝新回合（503）");
}

// ---------- R2 SSE + Last-Event-ID 断线续传 ----------

#[tokio::test]
async fn sse_last_event_id_resume_replays_without_loss() {
    let hub = event_stream::EventStreamHub::new();
    for i in 1..=5 {
        let _ = hub.publish_progress(format!("e{i}"));
    }
    // 断线前消费到 seq=2；重连按 Last-Event-ID=2 续传，零丢失。
    let (subscription, replay) = hub.subscribe_after(2);
    let _ = hub.publish_progress("e6");
    let mut seqs: Vec<u64> = replay.iter().map(|event| event.seq).collect();
    if let Some(event) = subscription.recv_blocking(Duration::from_millis(500)) {
        seqs.push(event.seq);
    }
    assert_eq!(seqs, vec![3, 4, 5, 6]);

    // 续传到最新 seq：无重放，只收实时。
    let (subscription, replay) = hub.subscribe_after(6);
    assert!(replay.is_empty(), "无新历史不应重放");
    let _ = hub.publish_progress("live");
    let event = subscription
        .recv_blocking(Duration::from_millis(500))
        .unwrap();
    assert_eq!(event.seq, 7);
}

#[tokio::test]
async fn sse_endpoint_serves_event_stream_and_resumes_with_last_event_id() {
    let (state, _temp) = test_state().await;
    let app = build_router(Arc::clone(&state));

    // 首次连接（last_event_id=0）。
    let first = app
        .clone()
        .oneshot(request(
            &state,
            "GET",
            "/events/stream?last_event_id=0",
            None,
        ))
        .await
        .unwrap();
    assert_eq!(first.status().as_u16(), 200, "首次 SSE 连接应 200");
    let first_ct = first
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        first_ct.contains("text/event-stream"),
        "Content-Type 应为 text/event-stream：{first_ct}"
    );

    // 断线后用更大 last_event_id 重连：仍 200 + SSE（无 4xx；续传语义由 hub 层测试覆盖）。
    let reconnect = app
        .clone()
        .oneshot(request(
            &state,
            "GET",
            "/events/stream?last_event_id=999999",
            None,
        ))
        .await
        .unwrap();
    assert_eq!(reconnect.status().as_u16(), 200, "断线重连应 200");
    let reconnect_ct = reconnect
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        reconnect_ct.contains("text/event-stream"),
        "重连 Content-Type 应为 text/event-stream：{reconnect_ct}"
    );
}

// ---------- R3 Idempotency-Key：重复请求不产生重复副作用 ----------

#[test]
fn idempotency_registry_duplicate_submission_records_side_effect_once() {
    let registry = idempotency::IdempotencyRegistry::new();
    let side_effects = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let first = registry.execute("op:order:1", Some("corr-1"), || {
        side_effects.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        idempotency::CachedResponse {
            status: 201,
            body: serde_json::json!({ "id": "order-1", "charged": true }),
            retry_after_ms: None,
            correlation_id: None,
        }
    });
    let second = registry.execute("op:order:1", Some("corr-1"), || {
        side_effects.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        panic!("重复提交不得再次产生副作用")
    });
    assert_eq!(first.status, second.status);
    assert_eq!(first.body, second.body, "重复提交返回首次结果");
    assert_eq!(
        side_effects.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "副作用恰好一次"
    );
    assert_eq!(registry.writes(), 1);
    assert_eq!(registry.hits(), 1);
}

#[tokio::test]
async fn fleet_submit_with_idempotency_key_creates_no_duplicate_task() {
    let (state, _temp) = test_state().await;
    let app = build_router(Arc::clone(&state));

    let task_id = format!("pr-{}", uuid::Uuid::new_v4());
    let body = format!(r#"{{"task_id":"{task_id}","worker":"node-a","input":{{}}}}"#);
    let headers = [("Idempotency-Key", task_id.as_str())];

    // 首次提交 → 200 + 回显幂等键。
    let first = app
        .clone()
        .oneshot(request_with_headers(
            &state,
            "POST",
            "/fleet/tasks/submit",
            Some(&body),
            &headers,
        ))
        .await
        .unwrap();
    assert_eq!(first.status().as_u16(), 200, "首次提交应 200");
    let first_json = body_json(first).await;
    assert_eq!(first_json["task_id"], task_id);
    assert_eq!(first_json["idempotency_key"], task_id, "响应应回显幂等键");

    // 同键同体重复提交 → 409：零重复副作用。
    let dup = app
        .clone()
        .oneshot(request_with_headers(
            &state,
            "POST",
            "/fleet/tasks/submit",
            Some(&body),
            &headers,
        ))
        .await
        .unwrap();
    assert_eq!(
        dup.status().as_u16(),
        409,
        "同幂等键重复提交应 409（不产生重复任务）"
    );
    let dup_json = body_json(dup).await;
    assert!(
        dup_json["error"]
            .as_str()
            .unwrap_or("")
            .contains("重复提交"),
        "409 错误体应说明重复提交"
    );

    // 查询：仍为单一任务，语义完整。
    let get = app
        .clone()
        .oneshot(request(
            &state,
            "GET",
            &format!("/fleet/tasks/{task_id}"),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(get.status().as_u16(), 200);
    let get_json = body_json(get).await;
    assert_eq!(get_json["task_id"], task_id);
}

// ---------- R4 存储 backup / export / clear ----------

#[tokio::test]
async fn storage_backup_export_clear_confirmation_and_integrity() {
    let (state, _temp) = test_state().await;
    let app = build_router(Arc::clone(&state));

    // 造一个会话，使导出 / 清空路径非空。
    let created = app
        .clone()
        .oneshot(request(
            &state,
            "POST",
            "/session",
            Some(r#"{"workspace":".","model":"idle"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(created.status().as_u16(), 200);

    // backup：zip 打包，manifest 声明含 index.db，且 index.db 条目非空。
    let backup = app
        .clone()
        .oneshot(request(&state, "POST", "/storage/backup", Some("{}")))
        .await
        .unwrap();
    assert_eq!(backup.status().as_u16(), 200, "backup 应 200");
    let backup_json = body_json(backup).await;
    assert_eq!(backup_json["ok"], true);
    assert!(
        backup_json["saved_to"]
            .as_str()
            .unwrap()
            .contains("backup-"),
        "saved_to 应指向 backups/backup-*.zip"
    );
    use std::io::Read;
    let archive_b64 = backup_json["archive_b64"].as_str().unwrap();
    let bytes =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, archive_b64).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
    let mut manifest = String::new();
    archive
        .by_name("manifest.json")
        .unwrap()
        .read_to_string(&mut manifest)
        .unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    assert_eq!(manifest["kind"], "backup");
    assert!(
        manifest["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry == "index.db"),
        "备份清单应含 index.db"
    );
    let mut index_db = Vec::new();
    archive
        .by_name("index.db")
        .unwrap()
        .read_to_end(&mut index_db)
        .unwrap();
    assert!(!index_db.is_empty(), "备份内的 index.db 应非空");

    // export：全量标准 JSON，含会话。
    let export = app
        .clone()
        .oneshot(request(&state, "POST", "/storage/export", Some("{}")))
        .await
        .unwrap();
    assert_eq!(export.status().as_u16(), 200, "export 应 200");
    let export_json = body_json(export).await;
    assert_eq!(export_json["format_version"], 1);
    for section in [
        "sessions",
        "audit",
        "notes",
        "skills",
        "workflows",
        "settings",
    ] {
        assert!(export_json.get(section).is_some(), "导出应含 {section}");
    }
    assert!(
        !export_json["sessions"].as_array().unwrap().is_empty(),
        "已建会话应出现在导出中"
    );

    // clear：缺二次确认 → 400。
    let no_confirm = app
        .clone()
        .oneshot(request(&state, "POST", "/storage/clear", Some("{}")))
        .await
        .unwrap();
    assert_eq!(no_confirm.status().as_u16(), 400, "缺确认应 400");

    // clear：确认后完整性 ok，且会话列表为空。
    let cleared = app
        .clone()
        .oneshot(request(
            &state,
            "POST",
            "/storage/clear",
            Some(r#"{"confirm":"CLEAR_ALL"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(cleared.status().as_u16(), 200);
    let cleared_json = body_json(cleared).await;
    assert_eq!(cleared_json["integrity"], "ok", "清空后完整性校验必须通过");
    assert!(
        cleared_json["cleared"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry == "sessions"),
        "清空清单应含 sessions"
    );

    let sessions = app
        .clone()
        .oneshot(request(&state, "GET", "/sessions", None))
        .await
        .unwrap();
    assert_eq!(sessions.status().as_u16(), 200);
    let list = body_json(sessions).await;
    assert_eq!(list.as_array().unwrap().len(), 0, "清空后会话列表应为空");
}
