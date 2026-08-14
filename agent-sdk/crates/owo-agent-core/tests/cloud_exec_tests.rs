//! M4 云端执行契约测试：v0.1 隔离执行/diff/revert + v0.2 队列/恢复/重试/传输/凭据不落盘/审计。

use owo_agent_core::cloud_exec::{CloudExecutor, CloudTaskSpec, DiffKind, LocalSimExecutor};
use std::path::PathBuf;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("owo-cloud-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(rel: &PathBuf, content: &str) {
    if let Some(parent) = rel.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(rel, content).unwrap();
}

#[tokio::test]
async fn run_task_returns_diff_and_leaves_original_untouched() {
    let ws = scratch("ws1");
    write(&ws.join("a.txt"), "old-content\n");
    write(&ws.join("keep.txt"), "keep\n");
    let ex = scratch("ex1");
    let mut exec = LocalSimExecutor::new(ex.clone());
    let task_id = exec
        .submit(CloudTaskSpec {
            name: "t".into(),
            workspace_dir: ws.clone(),
            commands: vec![
                "echo modified > a.txt".into(),
                "echo new > added.txt".into(),
            ],
            env_passthrough: vec![],
            timeout_secs: 30,
        })
        .unwrap();

    let result = exec.run(&task_id).await.unwrap();
    assert_eq!(result.exit_code, Some(0));
    // 原工作区不被改动（隔离）
    assert_eq!(
        std::fs::read_to_string(ws.join("a.txt")).unwrap(),
        "old-content\n"
    );
    assert!(!ws.join("added.txt").exists());
    // diff 回传：a.txt Modified、added.txt Added、keep.txt 不变
    assert!(result.diff.iter().any(|d| {
        d.path == "a.txt"
            && d.change == DiffKind::Modified
            && d.old.as_deref() == Some("old-content\n")
    }));
    assert!(result.diff.iter().any(|d| {
        d.path == "added.txt"
            && d.change == DiffKind::Added
            && d.new.as_deref().map(str::trim) == Some("new")
    }));
    assert!(!result.diff.iter().any(|d| d.path == "keep.txt"));

    // 把 diff 带回本地 → 应用 → revert → 恢复原状
    let local = scratch("apply1");
    std::fs::write(local.join("a.txt"), "old-content\n").unwrap();
    std::fs::write(local.join("keep.txt"), "keep\n").unwrap();
    result.apply_to(&local).unwrap();
    assert_eq!(
        std::fs::read_to_string(local.join("a.txt")).unwrap().trim(),
        "modified"
    );
    assert_eq!(
        std::fs::read_to_string(local.join("added.txt"))
            .unwrap()
            .trim(),
        "new"
    );
    result.revert_from(&local).unwrap();
    assert_eq!(
        std::fs::read_to_string(local.join("a.txt")).unwrap(),
        "old-content\n"
    );
    assert!(!local.join("added.txt").exists());
}

#[tokio::test]
async fn deleted_file_shows_in_diff_and_reverts() {
    let ws = scratch("ws2");
    write(&ws.join("gone.txt"), "to-be-deleted\n");
    let ex = scratch("ex2");
    let mut exec = LocalSimExecutor::new(ex.clone());
    let task_id = exec
        .submit(CloudTaskSpec {
            name: "del".into(),
            workspace_dir: ws.clone(),
            commands: vec!["del gone.txt".into()],
            env_passthrough: vec![],
            timeout_secs: 30,
        })
        .unwrap();
    let result = exec.run(&task_id).await.unwrap();
    let del = result
        .diff
        .iter()
        .find(|d| d.path == "gone.txt")
        .expect("应包含删除条目");
    assert_eq!(del.change, DiffKind::Deleted);
    assert_eq!(del.old.as_deref(), Some("to-be-deleted\n"));

    let local = scratch("apply2");
    std::fs::write(local.join("gone.txt"), "to-be-deleted\n").unwrap();
    result.apply_to(&local).unwrap();
    assert!(!local.join("gone.txt").exists());
    result.revert_from(&local).unwrap();
    assert_eq!(
        std::fs::read_to_string(local.join("gone.txt")).unwrap(),
        "to-be-deleted\n"
    );
}

#[tokio::test]
async fn task_isolation_two_tasks_no_crosstalk() {
    let ws = scratch("ws3");
    write(&ws.join("shared.txt"), "base\n");
    let ex = scratch("ex3");
    let mut exec = LocalSimExecutor::new(ex.clone());
    let t1 = exec
        .submit(CloudTaskSpec {
            name: "t1".into(),
            workspace_dir: ws.clone(),
            commands: vec!["echo one > mark.txt".into()],
            env_passthrough: vec![],
            timeout_secs: 30,
        })
        .unwrap();
    let t2 = exec
        .submit(CloudTaskSpec {
            name: "t2".into(),
            workspace_dir: ws.clone(),
            commands: vec!["echo two > mark.txt".into()],
            env_passthrough: vec![],
            timeout_secs: 30,
        })
        .unwrap();
    assert_ne!(t1, t2);
    let r1 = exec.run(&t1).await.unwrap();
    let r2 = exec.run(&t2).await.unwrap();
    // 各自隔离目录，互不覆盖
    assert_eq!(
        r1.diff
            .iter()
            .find(|d| d.path == "mark.txt")
            .unwrap()
            .new
            .as_deref()
            .map(str::trim),
        Some("one")
    );
    assert_eq!(
        r2.diff
            .iter()
            .find(|d| d.path == "mark.txt")
            .unwrap()
            .new
            .as_deref()
            .map(str::trim),
        Some("two")
    );
}

#[tokio::test]
async fn env_passthrough_whitelist_hides_secrets() {
    let ws = scratch("ws4");
    std::env::set_var("OWO_CLOUD_TEST_SECRET", "super-secret-value");
    std::env::set_var("OWO_CLOUD_TEST_ALLOWED", "visible-value");
    let ex = scratch("ex4");
    let mut exec = LocalSimExecutor::new(ex.clone());
    let task_id = exec
        .submit(CloudTaskSpec {
            name: "env".into(),
            workspace_dir: ws.clone(),
            commands: vec![
                "echo SECRET=%OWO_CLOUD_TEST_SECRET%".into(),
                "echo ALLOWED=%OWO_CLOUD_TEST_ALLOWED%".into(),
            ],
            env_passthrough: vec!["OWO_CLOUD_TEST_ALLOWED".into()],
            timeout_secs: 30,
        })
        .unwrap();
    let result = exec.run(&task_id).await.unwrap();
    // 凭据不落盘：白名单外的变量不会透传
    assert!(
        !result.stdout.contains("super-secret-value"),
        "秘密变量泄露：{}",
        result.stdout
    );
    assert!(
        result.stdout.contains("ALLOWED=visible-value"),
        "白名单变量应透传：{}",
        result.stdout
    );
}

#[tokio::test]
async fn revert_cleans_up_and_audits() {
    let ws = scratch("ws5");
    write(&ws.join("x.txt"), "x\n");
    let ex = scratch("ex5");
    let mut exec = LocalSimExecutor::new(ex.clone());
    let task_id = exec
        .submit(CloudTaskSpec {
            name: "cleanup".into(),
            workspace_dir: ws.clone(),
            commands: vec!["echo y > y.txt".into()],
            env_passthrough: vec![],
            timeout_secs: 30,
        })
        .unwrap();
    let _ = exec.run(&task_id).await.unwrap();
    let temp_dir = ex.join(&task_id);
    assert!(temp_dir.exists());
    exec.revert(&task_id).await.unwrap();
    assert!(!temp_dir.exists(), "隔离目录应被销毁");
    assert!(exec.run(&task_id).await.is_err(), "回滚后任务不可再执行");
    // 审计完整：submit / run / revert 三条
    let events: Vec<&str> = exec
        .audit()
        .entries
        .iter()
        .map(|e| e.event.as_str())
        .collect();
    assert!(events.contains(&"cloud.submit"));
    assert!(events.contains(&"cloud.run"));
    assert!(events.contains(&"cloud.revert"));
}

#[tokio::test]
async fn timeout_aborts_long_command_and_fails_task() {
    let ws = scratch("ws6");
    let ex = scratch("ex6");
    let mut exec = LocalSimExecutor::new(ex.clone());
    let task_id = exec
        .submit(CloudTaskSpec {
            name: "slow".into(),
            workspace_dir: ws.clone(),
            commands: vec!["ping -n 6 127.0.0.1".into()],
            env_passthrough: vec![],
            timeout_secs: 1,
        })
        .unwrap();
    let err = exec.run(&task_id).await.unwrap_err();
    assert!(err.contains("超时"), "应报超时：{err}");
}

#[tokio::test]
async fn malformed_specs_are_rejected() {
    let ws = scratch("ws7");
    let ex = scratch("ex7");
    let mut exec = LocalSimExecutor::new(ex.clone());
    assert!(exec
        .submit(CloudTaskSpec {
            name: "empty".into(),
            workspace_dir: ws.clone(),
            commands: vec![],
            env_passthrough: vec![],
            timeout_secs: 30,
        })
        .is_err());
    assert!(exec
        .submit(CloudTaskSpec {
            name: "inline-secret".into(),
            workspace_dir: ws.clone(),
            commands: vec!["echo hi".into()],
            env_passthrough: vec!["KEY=value".into()],
            timeout_secs: 30,
        })
        .is_err());
    assert!(exec
        .submit(CloudTaskSpec {
            name: "bad-dir".into(),
            workspace_dir: ex.join("nonexistent"),
            commands: vec!["echo hi".into()],
            env_passthrough: vec![],
            timeout_secs: 30,
        })
        .is_err());
}

// ==================== v0.2：队列 / 恢复 / 重试 / 传输 / 凭据 ====================

use owo_agent_core::cloud_exec::{
    backoff_delay, cloud_token_from_env, validate_commands, CloudProgress, CloudTaskQueue,
    CloudTransport, CollectingSink, HttpTransport, MockRemoteTransport, NullSink, TaskState,
};
use std::sync::Arc;

fn spec(workspace: PathBuf, commands: Vec<String>) -> CloudTaskSpec {
    CloudTaskSpec {
        name: "t".into(),
        workspace_dir: workspace,
        commands,
        env_passthrough: vec![],
        timeout_secs: 30,
    }
}

#[tokio::test]
async fn v02_end_to_end_mock_remote_apply_revert() {
    let ws = scratch("ws-v02");
    write(&ws.join("a.txt"), "old\n");
    let queue_dir = scratch("q-v02");
    let remote = MockRemoteTransport::new(scratch("remote-v02"));
    let mut queue = CloudTaskQueue::new(queue_dir.clone(), Box::new(remote));
    let sink = Arc::new(CollectingSink::new());

    let task_id = queue
        .submit(spec(
            ws.clone(),
            vec!["echo new > a.txt".into(), "echo added > b.txt".into()],
        ))
        .unwrap();
    let ran = queue
        .run_next(sink.as_ref() as &dyn owo_agent_core::cloud_exec::ProgressSink)
        .await
        .unwrap();
    assert_eq!(ran.as_deref(), Some(task_id.as_str()));

    let record = queue.record(&task_id).unwrap();
    assert_eq!(record.state, TaskState::Succeeded);
    let diff = queue.diff(&task_id).unwrap().to_vec();
    assert!(diff.iter().any(|d| d.path == "a.txt"));
    assert!(diff.iter().any(|d| d.path == "b.txt"));

    // 原工作区未被改动
    assert_eq!(std::fs::read_to_string(ws.join("a.txt")).unwrap(), "old\n");
    // apply → revert 往返
    queue.apply_to(&task_id, &ws).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(ws.join("a.txt")).unwrap().trim(),
        "new"
    );
    assert!(ws.join("b.txt").exists());
    queue.revert_from(&task_id, &ws).await.unwrap();
    assert_eq!(std::fs::read_to_string(ws.join("a.txt")).unwrap(), "old\n");
    assert!(!ws.join("b.txt").exists());

    // 进度事件序列完整
    let events = sink.all();
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| match e {
            CloudProgress::Snapshotting { .. } => "snapshotting",
            CloudProgress::Submitting { .. } => "submitting",
            CloudProgress::Submitted { .. } => "submitted",
            CloudProgress::Executing { .. } => "executing",
            CloudProgress::Fetching { .. } => "fetching",
            CloudProgress::Succeeded { .. } => "succeeded",
            CloudProgress::Failed { .. } => "failed",
            CloudProgress::Retrying { .. } => "retrying",
            CloudProgress::Canceled { .. } => "canceled",
        })
        .collect();
    assert!(kinds.contains(&"snapshotting"));
    assert!(kinds.contains(&"submitted"));
    assert!(kinds.contains(&"executing"));
    assert!(kinds.contains(&"fetching"));
    assert!(kinds.contains(&"succeeded"));

    // 审计完整
    let events_audit: Vec<&str> = queue
        .audit()
        .entries
        .iter()
        .map(|e| e.event.as_str())
        .collect();
    assert!(events_audit.contains(&"cloud.submit"));
    assert!(events_audit.contains(&"cloud.apply"));
    assert!(events_audit.contains(&"cloud.revert"));
}

#[tokio::test]
async fn v02_credentials_never_persist() {
    // 用 fallback 变量（OWO_CLOUD_API_KEY）避免与 http 契约测试并行争用 OWO_CLOUD_TOKEN。
    std::env::set_var("OWO_CLOUD_API_KEY", "super-secret-token-xyz");
    let queue_dir = scratch("q-cred");
    let remote = MockRemoteTransport::new(scratch("remote-cred"));
    let mut queue = CloudTaskQueue::new(queue_dir.clone(), Box::new(remote));
    let ws = scratch("ws-cred");
    write(&ws.join("x.txt"), "x\n");
    let _task_id = queue
        .submit(spec(ws.clone(), vec!["echo hi".into()]))
        .unwrap();
    queue.run_next(&NullSink).await.unwrap();

    // 持久化 JSON 不得含凭据
    for entry in std::fs::read_dir(&queue_dir).unwrap() {
        let content = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        assert!(!content.contains("super-secret-token-xyz"), "凭据落盘！");
        assert!(
            !content.contains("Authorization"),
            "持久化含 Authorization！"
        );
        assert!(!content.contains("token"), "持久化含 token 字段！");
    }
    // 审计同样不得含凭据
    let audit_text = format!("{:?}", queue.audit().entries);
    assert!(!audit_text.contains("super-secret-token-xyz"));
    // token 只读函数可用（fallback 语义）
    assert_eq!(
        cloud_token_from_env().as_deref(),
        Some("super-secret-token-xyz")
    );
    std::env::remove_var("OWO_CLOUD_API_KEY");
}

#[tokio::test]
async fn v02_queue_recover_after_restart() {
    let ws = scratch("ws-rec");
    write(&ws.join("x.txt"), "x\n");
    let queue_dir = scratch("q-rec");

    {
        let remote = MockRemoteTransport::new(scratch("remote-rec"));
        let mut queue = CloudTaskQueue::new(queue_dir.clone(), Box::new(remote));
        let id1 = queue
            .submit(spec(ws.clone(), vec!["echo one > f1.txt".into()]))
            .unwrap();
        let id2 = queue
            .submit(spec(ws.clone(), vec!["echo two > f2.txt".into()]))
            .unwrap();
        queue.run_next(&NullSink).await.unwrap(); // id1 成功
        assert_eq!(queue.record(&id1).unwrap().state, TaskState::Succeeded);
        assert_eq!(queue.record(&id2).unwrap().state, TaskState::Queued);
        drop(queue);
    }
    // 重启恢复
    let remote = MockRemoteTransport::new(scratch("remote-rec2"));
    let mut queue = CloudTaskQueue::new(queue_dir.clone(), Box::new(remote));
    let recovered = queue.recover().unwrap();
    assert!(recovered >= 2);
    assert_eq!(
        queue.record("cloud-0001").unwrap().state,
        TaskState::Succeeded
    );
    // Queued 任务重启后可继续执行
    let ran = queue.run_next(&NullSink).await.unwrap();
    assert_eq!(ran.as_deref(), Some("cloud-0002"));
    assert_eq!(
        queue.record("cloud-0002").unwrap().state,
        TaskState::Succeeded
    );
    assert!(queue
        .diff("cloud-0002")
        .unwrap()
        .iter()
        .any(|d| d.path == "f2.txt"));
}

#[tokio::test]
async fn v02_running_record_recovered_as_queued() {
    let queue_dir = scratch("q-rec-run");
    std::fs::create_dir_all(&queue_dir).unwrap();
    let ws = scratch("ws-rec-run");
    write(&ws.join("x.txt"), "x\n");
    // 手工构造 Running 记录并落盘
    let record = owo_agent_core::cloud_exec::TaskRecord {
        task_id: "cloud-0001".into(),
        remote_id: Some("remote-1".into()),
        spec: spec(ws.clone(), vec!["echo hi".into()]),
        state: TaskState::Running,
        retry_count: 0,
        last_error: None,
        result: None,
        created_at: "2026-08-14T00:00:00Z".into(),
        duration_ms: 0,
    };
    std::fs::write(
        queue_dir.join("cloud-0001.json"),
        serde_json::to_string_pretty(&record).unwrap(),
    )
    .unwrap();
    let remote = MockRemoteTransport::new(scratch("remote-rec-run"));
    let mut queue = CloudTaskQueue::new(queue_dir.clone(), Box::new(remote));
    queue.recover().unwrap();
    assert_eq!(
        queue.record("cloud-0001").unwrap().state,
        TaskState::Queued,
        "Running 任务重启后应重置为 Queued 可重跑"
    );
}

#[tokio::test]
async fn v02_retry_backoff_and_exhaustion() {
    // 退避纯函数
    assert_eq!(backoff_delay(1, 0).as_secs(), 1);
    assert_eq!(backoff_delay(1, 1).as_secs(), 2);
    assert_eq!(backoff_delay(1, 2).as_secs(), 4);
    assert_eq!(backoff_delay(2, 3).as_secs(), 16);
    assert!(backoff_delay(30, 99).as_secs() <= 60, "退避封顶 60s");

    // 失败任务重试：超限 → Failed
    let ws = scratch("ws-retry");
    write(&ws.join("x.txt"), "x\n");
    let queue_dir = scratch("q-retry");
    let remote = MockRemoteTransport::new(scratch("remote-retry"));
    // 命令必定失败（exit code 1）
    let mut queue = CloudTaskQueue::new(queue_dir.clone(), Box::new(remote)).with_max_retries(2);
    let task_id = queue
        .submit(spec(ws.clone(), vec!["exit /b 7".into()]))
        .unwrap();
    for _ in 0..3 {
        queue.run_next(&NullSink).await.unwrap();
    }
    let record = queue.record(&task_id).unwrap();
    assert_eq!(record.state, TaskState::Failed);
    assert_eq!(record.retry_count, 3, "1 次执行 + 2 次重试");
    // 审计含重试条目
    let events: Vec<&str> = queue
        .audit()
        .entries
        .iter()
        .map(|e| e.event.as_str())
        .collect();
    assert!(events.iter().filter(|e| **e == "cloud.retry").count() >= 2);

    // 未超限时回 Queued，可 retry 后重跑成功
    let ws2 = scratch("ws-retry2");
    write(&ws2.join("x.txt"), "x\n");
    let queue_dir2 = scratch("q-retry2");
    let remote2 = MockRemoteTransport::new(scratch("remote-retry2"));
    let mut queue2 = CloudTaskQueue::new(queue_dir2.clone(), Box::new(remote2)).with_max_retries(3);
    let task_id2 = queue2
        .submit(spec(ws2.clone(), vec!["exit /b 7".into()]))
        .unwrap();
    queue2.run_next(&NullSink).await.unwrap(); // 第 1 次失败 → 回 Queued
    assert_eq!(queue2.record(&task_id2).unwrap().state, TaskState::Queued);
    assert_eq!(queue2.record(&task_id2).unwrap().retry_count, 1);
}

#[tokio::test]
async fn v02_command_allowlist_and_dangerous_rejected() {
    assert!(validate_commands(&["echo hi".into()], &[]).is_ok());
    assert!(validate_commands(&["rm -rf /tmp/x".into()], &[]).is_err());
    assert!(validate_commands(&["shutdown /s".into()], &[]).is_err());
    assert!(validate_commands(&["format c: /q".into()], &[]).is_err());
    // 白名单外拒绝
    assert!(validate_commands(&["node x.js".into()], &["echo".into(), "git".into()]).is_err());
    assert!(validate_commands(&["git status".into()], &["git".into()]).is_ok());
    assert!(validate_commands(&["echo hi".into()], &["echo".into()]).is_ok());

    // 队列层校验：危险命令提交即拒绝
    let queue_dir = scratch("q-allow");
    let remote = MockRemoteTransport::new(scratch("remote-allow"));
    let mut queue = CloudTaskQueue::new(queue_dir.clone(), Box::new(remote));
    assert!(queue
        .submit(spec(scratch("ws-allow"), vec!["rm -rf /".into()]))
        .is_err());
}

#[tokio::test]
async fn v02_cancel_and_isolation() {
    let ws = scratch("ws-cancel");
    write(&ws.join("x.txt"), "x\n");
    let queue_dir = scratch("q-cancel");
    let remote = MockRemoteTransport::new(scratch("remote-cancel"));
    let mut queue = CloudTaskQueue::new(queue_dir.clone(), Box::new(remote));
    let id1 = queue
        .submit(spec(ws.clone(), vec!["echo one > m.txt".into()]))
        .unwrap();
    let id2 = queue
        .submit(spec(ws.clone(), vec!["echo two > m.txt".into()]))
        .unwrap();
    // 取消 id2（Queued）
    queue.cancel(&id2).await.unwrap();
    assert_eq!(queue.record(&id2).unwrap().state, TaskState::Canceled);
    // 执行 id1，互不影响
    queue.run_next(&NullSink).await.unwrap();
    assert_eq!(queue.record(&id1).unwrap().state, TaskState::Succeeded);
    assert_eq!(queue.record(&id2).unwrap().state, TaskState::Canceled);
    // 已取消任务不可执行
    let ran = queue.run_next(&NullSink).await.unwrap();
    assert!(ran.is_none(), "不应再执行 Canceled/其他任务");
    // 审计
    let events: Vec<&str> = queue
        .audit()
        .entries
        .iter()
        .map(|e| e.event.as_str())
        .collect();
    assert!(events.contains(&"cloud.cancel"));
    // 任务隔离：diff 内容各自独立（id1 的 m.txt = one）
    let diff = queue.diff(&id1).unwrap();
    let m = diff.iter().find(|d| d.path == "m.txt").unwrap();
    assert_eq!(m.new.as_deref().map(str::trim), Some("one"));
}

#[tokio::test]
async fn v02_http_transport_unreachable_clear_error() {
    let transport = HttpTransport::new("http://127.0.0.1:1".to_string()).unwrap();
    let spec = spec(scratch("ws-http"), vec!["echo hi".into()]);
    let err = transport.submit(&spec).await.unwrap_err();
    assert!(
        err.contains("失败") && err.contains("127.0.0.1:1"),
        "真实网络失败应有清晰报错：{err}"
    );
    // https（P2）：reqwest 内置 default-tls，https 远端应可构造（不再拒绝）。
    assert!(
        HttpTransport::new("https://example.com".into()).is_ok(),
        "https 由 reqwest default-tls 承载，应可构造"
    );
}

#[tokio::test]
async fn v02_http_transport_contract_against_inline_server() {
    // 极简 HTTP 远端：验证 submit/status/result/cancel 契约与 Authorization 头透传。
    std::env::set_var("OWO_CLOUD_TOKEN", "tok-abc");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for _ in 0..4 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let n = socket.read(&mut buf).await.unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            // 记录是否带 Authorization
            let has_auth = request.contains("Bearer tok-abc");
            let body = if request.contains("POST /cloud/tasks ") && has_auth {
                r#"{"id":"remote-7"}"#.to_string()
            } else if request.contains("GET /cloud/tasks/remote-7/result") {
                r#"{"task_id":"remote-7","exit_code":0,"stdout":"ok","stderr":"","diff":[],"diff_truncated":false}"#.to_string()
            } else if request.contains("POST /cloud/tasks/remote-7/cancel") {
                r#"{"ok":true}"#.to_string()
            } else {
                r#"{"state":"succeeded"}"#.to_string()
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let transport = HttpTransport::new(format!("http://{addr}")).unwrap();
    let spec = spec(scratch("ws-http2"), vec!["echo hi".into()]);
    let remote_id = transport.submit(&spec).await.unwrap();
    assert_eq!(remote_id, "remote-7");
    let status = transport.status(&remote_id).await.unwrap();
    assert_eq!(status, owo_agent_core::cloud_exec::RemoteStatus::Succeeded);
    let result = transport.fetch_result(&remote_id).await.unwrap();
    assert_eq!(result.exit_code, Some(0));
    transport.cancel(&remote_id).await.unwrap();
    server.await.unwrap();
    std::env::remove_var("OWO_CLOUD_TOKEN");
}

// M4a 云端执行 P2 契约测试：断线重连、多文件合并 diff、成本/时长计量、https 传输。
// 追加在 cloud_exec_tests.rs 尾部（主控授权 A4 补充）。

use owo_agent_core::cloud_exec::{
    describe_diff, validate_batch, CloudTaskResult, FileDiff, RemoteStatus, UsageMetrics,
};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

fn p2_scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("owo-cloud-p2-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn p2_write(rel: &Path, content: &str) {
    if let Some(parent) = rel.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(rel, content).unwrap();
}

fn p2_spec(ws: &Path) -> CloudTaskSpec {
    CloudTaskSpec {
        name: "p2".into(),
        workspace_dir: ws.to_path_buf(),
        commands: vec![
            "echo modified > a.txt".into(),
            "echo new > added.txt".into(),
        ],
        env_passthrough: vec![],
        timeout_secs: 30,
    }
}

/// 断线重连替身：前 N 次 status 调用返回瞬时错误，之后透传真实传输。
struct FlakyTransport {
    inner: MockRemoteTransport,
    fail_remaining: AtomicU32,
}

impl FlakyTransport {
    fn new(scratch: PathBuf, fail_times: u32) -> Self {
        Self {
            inner: MockRemoteTransport::new(scratch),
            fail_remaining: AtomicU32::new(fail_times),
        }
    }
}

#[async_trait::async_trait]
impl CloudTransport for FlakyTransport {
    fn kind(&self) -> &'static str {
        "flaky"
    }

    async fn submit(&self, spec: &CloudTaskSpec) -> Result<String, String> {
        self.inner.submit(spec).await
    }

    async fn status(&self, remote_id: &str) -> Result<RemoteStatus, String> {
        if self.fail_remaining.fetch_sub(1, Ordering::SeqCst) > 0 {
            return Err("瞬时网络中断（测试注入）".to_string());
        }
        self.inner.status(remote_id).await
    }

    async fn fetch_result(&self, remote_id: &str) -> Result<CloudTaskResult, String> {
        self.inner.fetch_result(remote_id).await
    }

    async fn cancel(&self, remote_id: &str) -> Result<(), String> {
        self.inner.cancel(remote_id).await
    }
}

#[tokio::test]
async fn p02_poll_reconnect_retries_transient_errors() {
    let ws = p2_scratch("ws-reconnect");
    p2_write(&ws.join("a.txt"), "old\n");
    let dir = p2_scratch("queue-reconnect");
    let scratch = p2_scratch("remote-reconnect");
    let mut queue =
        CloudTaskQueue::new(dir.join("queue"), Box::new(FlakyTransport::new(scratch, 2)));
    let sink = CollectingSink::new();
    let task_id = queue.submit(p2_spec(&ws)).unwrap();
    let finished = queue.run_next(&sink).await.unwrap();
    assert_eq!(finished.as_deref(), Some(task_id.as_str()));
    assert_eq!(
        queue.record(&task_id).unwrap().state,
        TaskState::Succeeded,
        "瞬时传输错误应被退避重试吸收，任务最终成功"
    );
    let events = sink.all();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CloudProgress::Retrying { .. })),
        "应发出 Retrying 进度事件"
    );
}

#[tokio::test]
async fn p02_diff_describe_and_path_validation() {
    let diffs = vec![
        FileDiff {
            path: "src/lib.rs".into(),
            change: DiffKind::Modified,
            old: Some("old".into()),
            new: Some("new".into()),
        },
        FileDiff {
            path: "README.md".into(),
            change: DiffKind::Added,
            old: None,
            new: Some("# readme".into()),
        },
        FileDiff {
            path: "old_main.rs".into(),
            change: DiffKind::Deleted,
            old: Some("x".into()),
            new: None,
        },
    ];
    let summary = describe_diff(&diffs);
    assert!(summary.contains("修改 1"));
    assert!(summary.contains("src/lib.rs"));
    assert!(summary.contains("新增 1"));
    assert!(summary.contains("README.md"));
    assert!(summary.contains("删除 1"));
    assert!(summary.contains("old_main.rs"));

    // 合法路径全部通过预校验。
    let root = p2_scratch("ws-desc");
    assert!(validate_batch(&diffs, &root).is_ok());
    // 越界路径（.. 跳转/绝对路径）整体拒绝。
    let escape = FileDiff {
        path: "../evil.txt".into(),
        change: DiffKind::Added,
        old: None,
        new: Some("boom".into()),
    };
    assert!(validate_batch(std::slice::from_ref(&escape), &root).is_err());
    let absolute = FileDiff {
        path: "C:\\Windows\\evil.txt".into(),
        change: DiffKind::Added,
        old: None,
        new: Some("boom".into()),
    };
    assert!(validate_batch(&[absolute], &root).is_err());
    // apply 直接调用同样拒绝越界路径。
    assert!(escape.apply(&root).is_err());
    assert!(!root.parent().unwrap().join("evil.txt").exists());
}

#[tokio::test]
async fn p02_usage_metrics_duration_and_diff_count() {
    let ws = p2_scratch("ws-usage");
    p2_write(&ws.join("a.txt"), "old\n");
    let dir = p2_scratch("queue-usage");
    let scratch = p2_scratch("remote-usage");
    let mut queue = CloudTaskQueue::new(
        dir.join("queue"),
        Box::new(MockRemoteTransport::new(scratch)),
    );
    let task_id = queue.submit(p2_spec(&ws)).unwrap();
    queue.run_next(&NullSink).await.unwrap();

    let usage: Option<UsageMetrics> = queue.usage(&task_id);
    let usage = usage.expect("任务完成后应有用量计量");
    assert!(usage.duration_ms > 0, "duration_ms 应实测非零");
    assert_eq!(usage.diff_count, 2, "a.txt Modified + added.txt Added");
    assert_eq!(usage.retry_count, 0);
    assert!(queue.record(&task_id).unwrap().duration_ms > 0);
}

#[tokio::test]
async fn p02_https_transport_accepted_and_scheme_validated() {
    // reqwest 内置 default-tls：https 远端应可构造（不再拒绝）。
    assert!(HttpTransport::new("https://example.com/cloud".to_string()).is_ok());
    assert!(HttpTransport::new("http://127.0.0.1:8080/cloud".to_string()).is_ok());
    // 非 http(s) scheme 明确拒绝。
    let error = match HttpTransport::new("ftp://example.com".to_string()) {
        Ok(_) => panic!("ftp scheme 应被拒绝"),
        Err(e) => e,
    };
    assert!(error.contains("http:// 或 https://"));
}

#[tokio::test]
async fn p02_cloud_result_batch_apply_revert_with_escape_guard() {
    let ws = p2_scratch("ws-batch");
    p2_write(&ws.join("keep.txt"), "keep\n");
    let good = FileDiff {
        path: "a.txt".into(),
        change: DiffKind::Modified,
        old: Some("old".into()),
        new: Some("new".into()),
    };
    let result = CloudTaskResult {
        task_id: "r1".into(),
        exit_code: Some(0),
        stdout: String::new(),
        stderr: String::new(),
        diff: vec![good.clone()],
        diff_truncated: false,
    };
    assert_eq!(result.apply_to(&ws).unwrap(), 1);
    assert_eq!(std::fs::read_to_string(ws.join("a.txt")).unwrap(), "new");
    assert_eq!(result.revert_from(&ws).unwrap(), 1);
    assert_eq!(std::fs::read_to_string(ws.join("a.txt")).unwrap(), "old");

    // 含越界条目的批次：apply 前置校验失败，0 应用且不写盘。
    let evil = FileDiff {
        path: "../escape.txt".into(),
        change: DiffKind::Added,
        old: None,
        new: Some("boom".into()),
    };
    let evil_result = CloudTaskResult {
        task_id: "r2".into(),
        exit_code: Some(0),
        stdout: String::new(),
        stderr: String::new(),
        diff: vec![good.clone(), evil.clone()],
        diff_truncated: false,
    };
    assert!(evil_result.apply_to(&ws).is_err());
    assert!(
        !ws.join("a.txt").exists() || std::fs::read_to_string(ws.join("a.txt")).unwrap() != "new"
    );
    assert!(!ws.parent().unwrap().join("escape.txt").exists());
}
