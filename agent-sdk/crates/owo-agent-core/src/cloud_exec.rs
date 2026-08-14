//! M4 云端执行骨架（v0.1）
//!
//! 全链路契约：仓库快照 → 隔离执行 → diff 回传 → revert；凭据不落盘、任务间隔离、审计完整。
//!
//! v0.1 提供本地模拟执行器 `LocalSimExecutor`（零外部依赖，Windows cmd / Unix sh），
//! 跑通并测试 M4 的验收契约；后续可新增 SSH / 容器传输实现 `CloudExecutor` trait。
//!
//! 安全基线：
//! - `CloudTaskSpec` 不携带任何凭据字段；子进程环境只透传 `env_passthrough` 白名单变量。
//! - 每个任务在工作区快照的隔离副本中执行，互不干扰，原工作区在显式 `apply_to` 前不被改动。
//! - 提交/执行/回滚全程写审计日志。

use crate::audit::AuditLog;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 单文件变更（diff 的基本单元，可序列化回传/审阅/应用）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub change: DiffKind,
    /// 变更前内容（Deleted/Modified 时存在）。
    pub old: Option<String>,
    /// 变更后内容（Added/Modified 时存在）。
    pub new: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DiffKind {
    Added,
    Modified,
    Deleted,
}

impl FileDiff {
    /// 正向应用：把 root 下的文件从 old 推进到 new（新增/覆盖/删除）。
    pub fn apply(&self, root: &Path) -> Result<(), String> {
        let target = root.join(&self.path);
        match self.change {
            DiffKind::Added | DiffKind::Modified => {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("创建目录 {} 失败：{e}", parent.display()))?;
                }
                let content = self.new.as_deref().unwrap_or("");
                std::fs::write(&target, content)
                    .map_err(|e| format!("写入 {} 失败：{e}", target.display()))
            }
            DiffKind::Deleted => {
                if target.exists() {
                    std::fs::remove_file(&target)
                        .map_err(|e| format!("删除 {} 失败：{e}", target.display()))?;
                }
                Ok(())
            }
        }
    }

    /// 反向应用（revert）：把 root 下的文件恢复到 old 状态。
    pub fn reverse(&self, root: &Path) -> Result<(), String> {
        let reverted = match self.change {
            DiffKind::Added => FileDiff {
                path: self.path.clone(),
                change: DiffKind::Deleted,
                old: None,
                new: None,
            },
            DiffKind::Deleted => FileDiff {
                path: self.path.clone(),
                change: DiffKind::Added,
                old: None,
                new: self.old.clone(),
            },
            DiffKind::Modified => FileDiff {
                path: self.path.clone(),
                change: DiffKind::Modified,
                old: None,
                new: self.old.clone(),
            },
        };
        reverted.apply(root)
    }
}

/// 云端任务规格。注意：不提供任何凭据字段。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudTaskSpec {
    pub name: String,
    /// 快照来源目录（v0.1 本地模拟时复制到隔离目录执行）。
    pub workspace_dir: PathBuf,
    /// 按序执行的命令。
    pub commands: Vec<String>,
    /// 环境变量白名单：只透传这些名字（值取自进程环境，不回写 spec）。
    pub env_passthrough: Vec<String>,
    /// 单条命令超时（秒）。
    pub timeout_secs: u64,
}

/// 任务执行结果：diff 携带回本地审阅/应用。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudTaskResult {
    pub task_id: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub diff: Vec<FileDiff>,
    /// diff 超过上限被截断时置 true。
    pub diff_truncated: bool,
}

impl CloudTaskResult {
    /// 把 diff 正向应用到 root（“带回本地”）。失败即中断，返回已应用条目以便回滚。
    pub fn apply_to(&self, root: &Path) -> Result<usize, (usize, String)> {
        for (i, d) in self.diff.iter().enumerate() {
            d.apply(root).map_err(|e| (i, e))?;
        }
        Ok(self.diff.len())
    }

    /// 回滚已经应用到 root 的 diff（与 apply_to 成对使用）。
    pub fn revert_from(&self, root: &Path) -> Result<usize, String> {
        let mut applied = 0usize;
        for d in self.diff.iter().rev() {
            d.reverse(root)
                .map_err(|e| format!("回滚 {} 失败:{e}", d.path))?;
            applied += 1;
        }
        Ok(applied)
    }
}

/// 任务句柄：submit 后持有，run 后携带结果。
#[derive(Debug)]
pub struct CloudTask {
    pub task_id: String,
    pub spec: CloudTaskSpec,
    pub(crate) temp_dir: Option<PathBuf>,
    pub result: Option<CloudTaskResult>,
}

/// 执行器抽象：submit → run → revert。未来可加 SSH/容器实现。
#[async_trait::async_trait]
pub trait CloudExecutor: Send {
    /// 提交任务并返回 task_id（v0.1 同步完成快照与隔离目录创建）。
    fn submit(&mut self, spec: CloudTaskSpec) -> Result<String, String>;
    /// 执行任务，产出 diff 结果。
    async fn run(&mut self, task_id: &str) -> Result<CloudTaskResult, String>;
    /// 回滚：销毁隔离执行环境并审计（原工作区由调用方经 apply/revert 控制）。
    async fn revert(&mut self, task_id: &str) -> Result<(), String>;
    /// 审计日志（只读）。
    fn audit(&self) -> &AuditLog;
}

const DIFF_LIMIT: usize = 200;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;

/// 本地模拟执行器：把工作区快照复制到 `scratch_root/<task_id>/` 隔离执行。
pub struct LocalSimExecutor {
    scratch_root: PathBuf,
    tasks: BTreeMap<String, CloudTask>,
    audit: AuditLog,
    counter: u64,
}

impl LocalSimExecutor {
    pub fn new(scratch_root: PathBuf) -> Self {
        Self {
            scratch_root,
            tasks: BTreeMap::new(),
            audit: AuditLog::default(),
            counter: 0,
        }
    }

    fn snapshot(root: &Path) -> Result<BTreeMap<String, String>, String> {
        let mut snap = BTreeMap::new();
        let mut total: u64 = 0;
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let entries = std::fs::read_dir(&dir)
                .map_err(|e| format!("读取目录 {} 失败：{e}", dir.display()))?;
            for entry in entries {
                let entry = entry.map_err(|e| format!("读取目录项失败：{e}"))?;
                let path = entry.path();
                let rel = path
                    .strip_prefix(root)
                    .map_err(|_| "路径越界".to_string())?
                    .to_string_lossy()
                    .replace('\\', "/");
                let kind = entry
                    .file_type()
                    .map_err(|e| format!("读取类型失败：{e}"))?;
                if kind.is_dir() {
                    stack.push(path);
                } else if kind.is_file() {
                    let meta = entry
                        .metadata()
                        .map_err(|e| format!("读取元数据失败：{e}"))?;
                    if meta.len() > MAX_FILE_BYTES {
                        return Err(format!("文件超过上限：{rel}（{}B）", meta.len()));
                    }
                    total += meta.len();
                    if total > MAX_SNAPSHOT_BYTES {
                        return Err(format!("快照总大小超过上限（{MAX_SNAPSHOT_BYTES}B）"));
                    }
                    let bytes = std::fs::read(&path)
                        .map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
                    snap.insert(rel, String::from_utf8_lossy(&bytes).to_string());
                }
            }
        }
        Ok(snap)
    }

    fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dst).map_err(|e| format!("创建 {} 失败：{e}", dst.display()))?;
        for entry in
            std::fs::read_dir(src).map_err(|e| format!("读取 {} 失败：{e}", src.display()))?
        {
            let entry = entry.map_err(|e| format!("读取目录项失败：{e}"))?;
            let kind = entry
                .file_type()
                .map_err(|e| format!("读取类型失败：{e}"))?;
            let from = entry.path();
            let to = dst.join(entry.file_name());
            if kind.is_dir() {
                Self::copy_tree(&from, &to)?;
            } else if kind.is_file() {
                std::fs::copy(&from, &to)
                    .map_err(|e| format!("复制 {} → {} 失败：{e}", from.display(), to.display()))?;
            }
        }
        Ok(())
    }

    /// 计算 after 相对 before 的 diff（按路径排序，超限截断）。
    fn compute_diff(before_root: &Path, after_root: &Path) -> (Vec<FileDiff>, bool) {
        let before = Self::snapshot(before_root).unwrap_or_default();
        let after = Self::snapshot(after_root).unwrap_or_default();
        let mut diff = Vec::new();
        let mut truncated = false;
        for (path, new_content) in &after {
            match before.get(path) {
                None => diff.push(FileDiff {
                    path: path.clone(),
                    change: DiffKind::Added,
                    old: None,
                    new: Some(new_content.clone()),
                }),
                Some(old) if old != new_content => diff.push(FileDiff {
                    path: path.clone(),
                    change: DiffKind::Modified,
                    old: Some(old.clone()),
                    new: Some(new_content.clone()),
                }),
                _ => {}
            }
        }
        for path in before.keys() {
            if !after.contains_key(path) {
                diff.push(FileDiff {
                    path: path.clone(),
                    change: DiffKind::Deleted,
                    old: before.get(path).cloned(),
                    new: None,
                });
            }
        }
        if diff.len() > DIFF_LIMIT {
            diff.truncate(DIFF_LIMIT);
            truncated = true;
        }
        (diff, truncated)
    }

    fn shell() -> (&'static str, &'static str) {
        if cfg!(windows) {
            ("cmd", "/C")
        } else {
            ("sh", "-c")
        }
    }
}

#[async_trait::async_trait]
impl CloudExecutor for LocalSimExecutor {
    fn submit(&mut self, spec: CloudTaskSpec) -> Result<String, String> {
        if spec.commands.is_empty() {
            return Err("任务至少需要一条命令".to_string());
        }
        if spec.env_passthrough.iter().any(|k| k.contains('=')) {
            return Err("env_passthrough 只允许变量名，禁止内联值".to_string());
        }
        if !spec.workspace_dir.is_dir() {
            return Err(format!(
                "工作区目录不存在：{}",
                spec.workspace_dir.display()
            ));
        }
        Self::snapshot(&spec.workspace_dir)?; // 先校验快照可读
        self.counter += 1;
        let task_id = format!("cloud-{:05}", self.counter);
        let temp_dir = self.scratch_root.join(&task_id);
        Self::copy_tree(&spec.workspace_dir, &temp_dir)?;
        let task = CloudTask {
            task_id: task_id.clone(),
            spec,
            temp_dir: Some(temp_dir),
            result: None,
        };
        self.audit.record(
            "cloud",
            "cloud.submit",
            None,
            None,
            format!(
                "task_id={task_id} 命令数={} 隔离目录已就绪",
                task.spec.commands.len()
            ),
        );
        self.tasks.insert(task_id.clone(), task);
        Ok(task_id)
    }

    async fn run(&mut self, task_id: &str) -> Result<CloudTaskResult, String> {
        let (workdir, commands, passthrough, timeout, workspace_dir) = {
            let task = self
                .tasks
                .get(task_id)
                .ok_or_else(|| format!("任务不存在：{task_id}"))?;
            (
                task.temp_dir
                    .clone()
                    .ok_or_else(|| format!("任务已回滚：{task_id}"))?,
                task.spec.commands.clone(),
                task.spec.env_passthrough.clone(),
                task.spec.timeout_secs,
                task.spec.workspace_dir.clone(),
            )
        };
        let (shell, flag) = Self::shell();
        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut exit_code = None;
        let mut timed_out = false;
        for cmd in &commands {
            let mut builder = tokio::process::Command::new(shell);
            builder.arg(flag).arg(cmd).current_dir(&workdir);
            builder.env_clear().kill_on_drop(true);
            // env_clear 后恒保留 PATH/SystemRoot（非机密），其余仅透传白名单。
            for keep in ["PATH", "SystemRoot", "COMSPEC"] {
                if let Ok(value) = std::env::var(keep) {
                    builder.env(keep, value);
                }
            }
            for key in &passthrough {
                if let Ok(value) = std::env::var(key) {
                    builder.env(key, value);
                }
            }
            let output = match tokio::time::timeout(
                std::time::Duration::from_secs(timeout),
                builder.output(),
            )
            .await
            {
                Ok(Ok(output)) => output,
                Ok(Err(e)) => return Err(format!("命令启动失败：{e}")),
                Err(_) => {
                    timed_out = true;
                    break;
                }
            };
            stdout.push_str(&String::from_utf8_lossy(&output.stdout));
            stderr.push_str(&String::from_utf8_lossy(&output.stderr));
            exit_code = output.status.code();
            if exit_code != Some(0) {
                break;
            }
        }
        let (diff, truncated) = Self::compute_diff(&workspace_dir, &workdir);
        let result = CloudTaskResult {
            task_id: task_id.to_string(),
            exit_code,
            stdout,
            stderr,
            diff,
            diff_truncated: truncated,
        };
        self.audit.record(
            "cloud",
            "cloud.run",
            None,
            None,
            format!(
                "task_id={task_id} 退出码={:?} diff 条目={} 截断={} 超时={}",
                result.exit_code,
                result.diff.len(),
                truncated,
                timed_out
            ),
        );
        if let Some(task) = self.tasks.get_mut(task_id) {
            task.result = Some(result.clone());
        }
        if timed_out {
            return Err(format!("命令执行超时（>{timeout}s）"));
        }
        Ok(result)
    }

    async fn revert(&mut self, task_id: &str) -> Result<(), String> {
        let task = self
            .tasks
            .remove(task_id)
            .ok_or_else(|| format!("任务不存在：{task_id}"))?;
        if let Some(temp_dir) = task.temp_dir {
            if temp_dir.exists() {
                std::fs::remove_dir_all(&temp_dir).map_err(|e| format!("清理隔离目录失败：{e}"))?;
            }
        }
        self.audit.record(
            "cloud",
            "cloud.revert",
            None,
            None,
            format!("task_id={task_id} 隔离环境已销毁，原工作区未被改动"),
        );
        Ok(())
    }

    fn audit(&self) -> &AuditLog {
        &self.audit
    }
}
