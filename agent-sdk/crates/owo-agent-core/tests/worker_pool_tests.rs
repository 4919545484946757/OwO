//! worker 子进程池契约测试（R8 简化版：保留主链路冒烟 + 关键故障路径）。
//!
//! 覆盖：spawn→ready 握手→结构化任务→结果→kill 无孤儿（主链路冒烟）、
//! 心跳、时长预算中止并 kill、连续崩溃熔断、取消传播、未知 worker 错误。
//! 子进程 = 本测试二进制自身（`--exact worker_child_entry` + 环境标记），
//! 协议实现复用 `owo_agent_core::worker_pool::child::run_child_protocol`。

use owo_agent_core::fleet::RestartRule;
use owo_agent_core::worker_pool::{
    child, IsolationMode, PoolError, WorkerBudget, WorkerPool, WorkerSpec, WorkerStatus,
};
use serde_json::{json, Value};
use std::time::Duration;

/// 子进程模式入口（父进程用 `--exact worker_child_entry --nocapture` + 环境标记拉起）。
#[test]
fn worker_child_entry() {
    if std::env::var("OWO_WORKER_CHILD").is_err() {
        return; // 父进程测试模式下直接返回
    }
    child::run_child_protocol(|input: &Value| {
        if input.get("crash").and_then(Value::as_bool).unwrap_or(false) {
            std::process::exit(42); // 任务中崩溃
        }
        if let Some(ms) = input.get("sleep_ms").and_then(Value::as_u64) {
            std::thread::sleep(Duration::from_millis(ms));
        }
        let text = input
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Ok(format!("out-{text}"))
    });
}

/// 构造指向本测试二进制的子进程规格（echo 协议模式）。
fn echo_spec(id: &str) -> WorkerSpec {
    WorkerSpec::new(id, std::env::current_exe().unwrap())
        .args(vec![
            "--exact".to_string(),
            "worker_child_entry".to_string(),
            "--nocapture".to_string(),
            "--quiet".to_string(),
        ])
        .env_whitelist(vec![("OWO_WORKER_CHILD".to_string(), "1".to_string())])
        .restart_rule(RestartRule {
            max_restarts: 3,
            base_backoff_secs: 0,
            policy: owo_agent_core::fleet::RestartPolicy::OneForOne,
        })
}

/// 用 tasklist 检查进程是否存活（Windows）。
async fn pid_alive(pid: u32) -> bool {
    let filter = format!("PID eq {pid}");
    let out = tokio::process::Command::new("tasklist")
        .args(["/FI", &filter])
        .output()
        .await;
    match out {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            text.contains(&pid.to_string())
        }
        Err(_) => false,
    }
}

/// 主链路冒烟：spawn → ready 握手 → 结构化任务 → 结果 → kill 无孤儿。
#[tokio::test]
async fn spawn_task_result_kill_no_orphan() {
    let pool = WorkerPool::new();
    pool.spawn(echo_spec("echo")).await.unwrap();
    assert!(pool.contains("echo").await);
    let out = pool.submit("echo", &json!({ "text": "A" })).await.unwrap();
    assert_eq!(out, "out-A", "结构化任务消息应原样回传结果");
    assert!(pool.pid("echo").await.is_some(), "应记录子进程 pid");
    let pid = pool.pid("echo").await.unwrap();
    // 心跳正常。
    pool.ping("echo").await.expect("心跳 ping/pong 应成功");
    // kill 后：状态 Stopped、心跳失败、进程无孤儿。
    pool.kill("echo").await.unwrap();
    assert_eq!(pool.status("echo").await, Some(WorkerStatus::Stopped));
    assert!(
        pool.ping("echo").await.is_err(),
        "已 kill：心跳必须报错而非挂起"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!pid_alive(pid).await, "kill 后子进程必须已终止（无孤儿）");
    pool.shutdown().await;
}

#[tokio::test]
async fn heartbeat_ping_pong_ok() {
    let pool = WorkerPool::new();
    pool.spawn(echo_spec("echo")).await.unwrap();
    match pool.ping("echo").await {
        Ok(()) => {}
        Err(e) => panic!("心跳 ping/pong 应成功：{e}"),
    }
    pool.kill("echo").await.unwrap();
    assert!(pool.ping("echo").await.is_err());
}

#[tokio::test]
async fn budget_max_duration_aborts_and_kills() {
    let pool = WorkerPool::new();
    let spec = echo_spec("slow").budget(WorkerBudget {
        max_duration_secs: 1,
        ..Default::default()
    });
    pool.spawn(spec).await.unwrap();
    let pid = pool.pid("slow").await.unwrap();
    let err = pool
        .submit("slow", &json!({ "text": "x", "sleep_ms": 5000 }))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PoolError::BudgetDuration { .. }),
        "时长预算到期应中止：{err}"
    );
    assert!(
        matches!(pool.status("slow").await, Some(WorkerStatus::Stopped)),
        "预算中止后 worker 应为 Stopped"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!pid_alive(pid).await, "预算中止必须 kill 子进程（无孤儿）");
    pool.shutdown().await;
}

#[tokio::test]
async fn repeated_crashes_fuse_circuit() {
    let pool = WorkerPool::new();
    let rule = RestartRule {
        max_restarts: 2,
        base_backoff_secs: 0,
        policy: owo_agent_core::fleet::RestartPolicy::OneForOne,
    };
    pool.spawn(echo_spec("echo").restart_rule(rule))
        .await
        .unwrap();
    for _ in 0..3 {
        let _ = pool
            .submit("echo", &json!({ "text": "x", "crash": true }))
            .await;
        let _ = pool.check_health("echo").await;
    }
    assert!(
        matches!(pool.status("echo").await, Some(WorkerStatus::Fused { .. })),
        "连续失败后应熔断：{:?}",
        pool.status("echo").await
    );
    let err = pool
        .submit("echo", &json!({ "text": "z" }))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PoolError::Fused(_)),
        "熔断后提交应被拒：{err}"
    );
    pool.shutdown().await;
}

#[tokio::test]
async fn cancel_pending_propagates_no_orphan() {
    let pool = WorkerPool::new();
    pool.spawn(echo_spec("echo")).await.unwrap();
    let pid = pool.pid("echo").await.unwrap();
    let submit = tokio::spawn({
        let pool = pool.clone();
        async move {
            pool.submit("echo", &json!({ "text": "x", "sleep_ms": 5000 }))
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let cancelled = pool.cancel_pending("echo").await.unwrap();
    assert_eq!(cancelled, 1, "应取消 1 个待处理任务");
    let err = submit.await.unwrap().unwrap_err();
    assert!(
        matches!(err, PoolError::Cancelled(_)),
        "取消传播应立即可见：{err}"
    );
    pool.kill("echo").await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!pid_alive(pid).await, "取消后 kill 不得留下孤儿进程");
}

#[tokio::test]
async fn unknown_worker_errors() {
    let pool = WorkerPool::new();
    assert!(matches!(
        pool.submit("nope", &json!({})).await.unwrap_err(),
        PoolError::UnknownWorker(_)
    ));
    assert!(matches!(
        pool.ping("nope").await.unwrap_err(),
        PoolError::UnknownWorker(_)
    ));
    assert!(matches!(
        pool.kill("nope").await.unwrap_err(),
        PoolError::UnknownWorker(_)
    ));
}

#[tokio::test]
async fn isolation_default_is_process() {
    assert_eq!(IsolationMode::default(), IsolationMode::Process);
    let spec = echo_spec("iso");
    assert_eq!(spec.isolation, IsolationMode::Process, "默认进程隔离");
}
