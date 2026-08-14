//! M4 云端执行骨架契约测试：隔离执行 / diff 回传 / revert / 凭据不落盘 / 任务隔离 / 审计。

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
