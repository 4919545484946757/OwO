//! OS 级执行沙箱（综合文档 §6 P0 / X01，Wave 2：Windows OS 边界落地）。
//!
//! - `SandboxPolicy` / `SandboxExecutor` / `SandboxManager`：策略校验 → 能力评估 →
//!   显式降级/拒绝 → 审计的统一入口（Wave 1 契约保留）。
//! - `probe_platform_support()`：Windows 真实能力探测（Job Object 创建、低完整性令牌、
//!   AppContainer API 存在性），探测失败一律按不可用处理并携带原因。
//! - `WindowsSandboxExecutor`：Job Object（CPU/内存/进程数上限 + kill-on-close 防孤儿）
//!   为基线；策略要求低完整性时经受限令牌 + 完整性标签创建进程；要求 AppContainer 且
//!   平台支持时经 `SECURITY_CAPABILITIES` 创建 AppContainer 进程。
//! - 硬性约定：**无法建立所需 OS 隔离时返回 `Unsupported` 并写入审计事件，
//!   创建失败一律显式失败（终止已启动进程），禁止静默假装安全**。
//! - Wave 1 的 `#[path]` 独立编译约定已结束（R6 主控已把本模块并入 lib.rs）。

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// 沙箱文件作用域。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileScope {
    /// 仅工作区内可读写（默认）。
    #[default]
    WorkspaceOnly,
    /// 工作区 + 只读系统路径。
    WorkspacePlusReadonlySystem,
    /// 无文件限制（高风险，默认拒绝，需显式放行）。
    Unrestricted,
}

/// 沙箱网络策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicy {
    /// 完全隔离（默认）。
    #[default]
    None,
    /// 仅本地回环。
    Loopback,
    /// 白名单 host（`allow_hosts`）。
    AllowList,
    /// 无限制（高风险，默认拒绝，需显式放行）。
    Unrestricted,
}

/// OS 隔离强度（按声明顺序递增：`None < LowIntegrity < JobOnly < AppContainerJob`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationLevel {
    /// 无 OS 级隔离。
    None,
    /// 低完整性（Low Integrity Level）。
    LowIntegrity,
    /// Job Object 资源限制（CPU/内存/生命周期）。
    JobOnly,
    /// Windows AppContainer + Job Object（完整方案）。
    #[default]
    AppContainerJob,
}

/// 沙箱策略：文件作用域 / 网络 / 资源上限 / 存活时间 / 隔离要求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxPolicy {
    /// 沙箱名（审计用）。
    pub name: String,
    /// 工作区根目录（`WorkspaceOnly` 必需）。
    pub workspace: Option<PathBuf>,
    pub file_scope: FileScope,
    pub network_policy: NetworkPolicy,
    /// CPU 时间上限（毫秒，`None` = 不限制，`Some(0)` 非法）。
    pub cpu_ms: Option<u64>,
    /// 内存上限（MB，`None` = 不限制）。
    pub mem_mb: Option<u64>,
    /// 存活时长上限（秒，`None` = 不限制）。
    pub ttl_secs: Option<u64>,
    /// `AllowList` 网络白名单（host 或 host:port）。
    pub allow_hosts: Vec<String>,
    /// 要求的隔离强度（默认 AppContainerJob）。
    pub require_isolation: IsolationLevel,
    /// 是否允许显式降级（如仅有 Job 无 AppContainer）。
    pub allow_degraded: bool,
    /// Job 内活动进程数上限（`Some(n)` 限制，防进程炸弹；`None` = 不限制）。
    pub active_process_limit: Option<u32>,
    /// 危险程序片段黑名单（deny 优先，大小写不敏感子串匹配）。
    pub deny_programs: Vec<String>,
    /// 显式放行无文件限制（高风险开关）。
    pub allow_unrestricted_file: bool,
    /// 显式放行无网络限制（高风险开关）。
    pub allow_unrestricted_network: bool,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            workspace: None,
            file_scope: FileScope::WorkspaceOnly,
            network_policy: NetworkPolicy::None,
            cpu_ms: Some(30_000),
            mem_mb: Some(1024),
            ttl_secs: Some(600),
            allow_hosts: Vec::new(),
            require_isolation: IsolationLevel::AppContainerJob,
            allow_degraded: false,
            active_process_limit: Some(1),
            deny_programs: vec![
                "shutdown".to_string(),
                "format".to_string(),
                "reg delete".to_string(),
            ],
            allow_unrestricted_file: false,
            allow_unrestricted_network: false,
        }
    }
}

impl SandboxPolicy {
    /// 默认工作区沙箱策略（只读默认）。
    pub fn for_workspace(name: impl Into<String>, workspace: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            workspace: Some(workspace.into()),
            ..SandboxPolicy::default()
        }
    }

    /// 策略自检：越界组合（未显式放行的高风险策略、非法资源值）一律拒绝。
    pub fn validate(&self) -> Result<(), SandboxError> {
        if self.file_scope == FileScope::Unrestricted && !self.allow_unrestricted_file {
            return Err(SandboxError::PolicyViolation(
                "文件作用域 Unrestricted 必须显式设置 allow_unrestricted_file".to_string(),
            ));
        }
        if self.network_policy == NetworkPolicy::Unrestricted && !self.allow_unrestricted_network {
            return Err(SandboxError::PolicyViolation(
                "网络策略 Unrestricted 必须显式设置 allow_unrestricted_network".to_string(),
            ));
        }
        if self.network_policy == NetworkPolicy::AllowList && self.allow_hosts.is_empty() {
            return Err(SandboxError::PolicyViolation(
                "AllowList 网络策略需要非空 allow_hosts".to_string(),
            ));
        }
        if self.cpu_ms == Some(0) {
            return Err(SandboxError::PolicyViolation(
                "cpu_ms 为 0 非法：None=不限制，Some(n>0)=限制".to_string(),
            ));
        }
        if self.file_scope == FileScope::WorkspaceOnly && self.workspace.is_none() {
            return Err(SandboxError::PolicyViolation(
                "WorkspaceOnly 文件作用域需要 workspace 路径".to_string(),
            ));
        }
        Ok(())
    }
}

/// 待执行的沙箱命令。
#[derive(Debug, Clone)]
pub struct SandboxCommand {
    pub program: String,
    pub args: Vec<String>,
    pub policy: SandboxPolicy,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
}

impl SandboxCommand {
    pub fn new(program: impl Into<String>, policy: SandboxPolicy) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            policy,
            cwd: None,
            env: Vec::new(),
        }
    }

    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// 危险片段命中检查（大小写不敏感子串；返回命中的片段）。
    pub fn deny_hit(text: &str, deny_programs: &[String]) -> Option<String> {
        let lower = text.to_lowercase();
        deny_programs
            .iter()
            .find(|fragment| lower.contains(&fragment.to_lowercase()))
            .cloned()
    }

    /// 命令级校验：策略自检 + 工作区越界 + 危险程序黑名单。
    pub fn validate(&self) -> Result<(), SandboxError> {
        self.policy.validate()?;
        if self.policy.file_scope == FileScope::WorkspaceOnly {
            if let (Some(cwd), Some(root)) = (&self.cwd, &self.policy.workspace) {
                if cwd.is_absolute() && !cwd.starts_with(root) {
                    return Err(SandboxError::PolicyViolation(format!(
                        "工作目录越界：{} 不在工作区 {} 内",
                        cwd.display(),
                        root.display()
                    )));
                }
            }
        }
        if let Some(fragment) = Self::deny_hit(&self.program, &self.policy.deny_programs) {
            return Err(SandboxError::PolicyViolation(format!(
                "程序命中危险黑名单片段：{}",
                fragment
            )));
        }
        Ok(())
    }
}

/// 沙箱进程句柄。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxHandle {
    pub id: String,
    pub spawned_at: String,
}

/// 沙箱进程状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxProcessStatus {
    Running,
    Exited(i32),
    Killed,
    Failed(String),
}

/// 进程退出信息（wait_output 结果）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxWaitInfo {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// 沙箱进程内部句柄（OS 执行器填充；Mock 为空）。
pub trait SandboxProcessInner: Send {
    /// 阻塞等待退出并采集输出。
    fn wait(&mut self) -> Result<SandboxWaitInfo, SandboxError>;
    /// 终止进程。
    fn kill(&mut self) -> Result<(), SandboxError>;
}

/// 沙箱进程。
pub struct SandboxProcess {
    pub handle: SandboxHandle,
    pub status: SandboxProcessStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub(crate) inner: Option<Box<dyn SandboxProcessInner>>,
}

impl std::fmt::Debug for SandboxProcess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SandboxProcess")
            .field("handle", &self.handle)
            .field("status", &self.status)
            .field("stdout_len", &self.stdout.len())
            .field("stderr_len", &self.stderr.len())
            .finish_non_exhaustive()
    }
}

impl SandboxProcess {
    /// 阻塞等待进程退出并采集输出（无 inner 时返回已缓存数据与状态码）。
    pub fn wait_output(&mut self) -> Result<SandboxWaitInfo, SandboxError> {
        if let Some(inner) = self.inner.as_mut() {
            let info = inner.wait()?;
            self.stdout = info.stdout.clone();
            self.stderr = info.stderr.clone();
            self.status = SandboxProcessStatus::Exited(info.exit_code);
            return Ok(info);
        }
        let exit_code = match self.status {
            SandboxProcessStatus::Exited(code) => code,
            SandboxProcessStatus::Killed => 1,
            SandboxProcessStatus::Failed(_) | SandboxProcessStatus::Running => -1,
        };
        Ok(SandboxWaitInfo {
            exit_code,
            stdout: std::mem::take(&mut self.stdout),
            stderr: std::mem::take(&mut self.stderr),
        })
    }

    /// 终止进程（Job 层兜底）。
    pub fn kill(&mut self) -> Result<(), SandboxError> {
        if let Some(inner) = self.inner.as_mut() {
            inner.kill()
        } else {
            Err(SandboxError::Kill("进程无内部句柄".to_string()))
        }
    }
}

/// 执行器健康状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxHealth {
    pub healthy: bool,
    pub detail: String,
}

/// 沙箱执行器接口。
pub trait SandboxExecutor: Send + Sync {
    fn name(&self) -> &'static str;
    /// 执行器实际可提供的隔离强度。
    fn capability(&self) -> IsolationLevel;
    fn spawn(&self, command: &SandboxCommand) -> Result<SandboxProcess, SandboxError>;
    fn kill(&self, handle: &SandboxHandle) -> Result<(), SandboxError>;
    fn check_healthy(&self) -> SandboxHealth;
    /// 把运行中的进程挂入受限 Job（默认不支持）。
    fn attach(&self, _policy: &SandboxPolicy, _pid: u32) -> Result<JobGuard, SandboxError> {
        Err(SandboxError::Unsupported(
            "该执行器不支持挂接运行中进程".to_string(),
        ))
    }
}

/// 沙箱错误：显式拒绝/降级，绝不静默。
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("沙箱不可用：{0}")]
    Unsupported(String),
    #[error("策略违规：{0}")]
    PolicyViolation(String),
    #[error("启动失败：{0}")]
    Spawn(String),
    #[error("终止失败：{0}")]
    Kill(String),
    #[error("沙箱不健康：{0}")]
    Unhealthy(String),
    #[error("io 错误：{0}")]
    Io(#[from] std::io::Error),
}

/// 平台 OS 能力探测结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformSupport {
    pub os: String,
    /// Windows AppContainer（Win8+）可用。
    pub app_container: bool,
    /// Job Object 可用。
    pub job_object: bool,
    /// 低完整性令牌可用。
    pub low_integrity: bool,
    /// 探测/降级原因（审计用）。
    pub reason: String,
}

/// 平台能力探测：Windows 真实探测（Job 创建 / 低完整性令牌 / AppContainer API）。
/// 任何一步无法验证即按不可用处理并写入 reason（禁止假装安全）。
/// 非 Windows（R10）：显式 `Unsupported` + 可审计降级原因——若检测到 bwrap（Linux）
/// 或 sandbox-exec（macOS）则写入 reason 供后续 Wave 接入，绝不编译期假装支持。
pub fn probe_platform_support() -> PlatformSupport {
    #[cfg(target_os = "windows")]
    {
        win::probe_windows_support()
    }
    #[cfg(not(target_os = "windows"))]
    {
        let os = std::env::consts::OS;
        let mut hints = Vec::new();
        #[cfg(target_os = "linux")]
        {
            if std::path::Path::new("/usr/bin/bwrap").exists()
                || std::path::Path::new("/bin/bwrap").exists()
            {
                hints.push("检测到 bwrap（Wave 接入点已预留）");
            }
        }
        #[cfg(target_os = "macos")]
        {
            if std::path::Path::new("/usr/bin/sandbox-exec").exists() {
                hints.push("检测到 sandbox-exec（Wave 接入点已预留）");
            }
        }
        let mut reason = format!("平台 {os} 暂不支持 OS 级执行沙箱（显式降级，不静默假装安全）");
        if !hints.is_empty() {
            reason.push('；');
            reason.push_str(&hints.join("；"));
        }
        PlatformSupport {
            os: os.to_string(),
            app_container: false,
            job_object: false,
            low_integrity: false,
            reason,
        }
    }
}

/// 平台可用隔离强度（按能力取最高档）。
pub fn available_isolation(support: &PlatformSupport) -> IsolationLevel {
    if support.app_container && support.job_object {
        IsolationLevel::AppContainerJob
    } else if support.job_object {
        IsolationLevel::JobOnly
    } else if support.low_integrity {
        IsolationLevel::LowIntegrity
    } else {
        IsolationLevel::None
    }
}

/// 能力评估结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityEvaluation {
    /// 满足策略要求的完整隔离。
    Full,
    /// 显式降级（策略允许 `allow_degraded`）。
    Degraded(IsolationLevel),
}

/// 能力评估：策略要求 vs 平台可用 → `Full` / 显式 `Degraded` / 显式 `Unsupported`。
pub fn evaluate_capability(
    support: &PlatformSupport,
    policy: &SandboxPolicy,
) -> Result<CapabilityEvaluation, SandboxError> {
    let available = available_isolation(support);
    if available == IsolationLevel::None {
        return Err(SandboxError::Unsupported(support.reason.clone()));
    }
    if available < policy.require_isolation {
        if policy.allow_degraded {
            return Ok(CapabilityEvaluation::Degraded(available));
        }
        return Err(SandboxError::Unsupported(format!(
            "需要 {:?} 隔离，平台仅提供 {:?}（显式 allow_degraded 可降级）",
            policy.require_isolation, available
        )));
    }
    Ok(CapabilityEvaluation::Full)
}

/// 沙箱审计事件类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxEventKind {
    CapabilityProbe,
    SpawnRejected,
    UnsupportedIsolation,
    DegradedIsolation,
    Killed,
    Unhealthy,
    Attached,
    EgressRejected,
    PluginRejected,
}

impl SandboxEventKind {
    /// 审计链事件名后缀（sandbox.<label>）。
    pub fn label(&self) -> &'static str {
        match self {
            SandboxEventKind::CapabilityProbe => "capability_probe",
            SandboxEventKind::SpawnRejected => "spawn_rejected",
            SandboxEventKind::UnsupportedIsolation => "unsupported_isolation",
            SandboxEventKind::DegradedIsolation => "degraded_isolation",
            SandboxEventKind::Killed => "killed",
            SandboxEventKind::Unhealthy => "unhealthy",
            SandboxEventKind::Attached => "attached",
            SandboxEventKind::EgressRejected => "egress_rejected",
            SandboxEventKind::PluginRejected => "plugin_rejected",
        }
    }
}

/// 沙箱审计事件（append-only，可汇入 audit_chain）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxAuditEvent {
    pub ts: String,
    pub kind: SandboxEventKind,
    pub sandbox: String,
    pub detail: String,
}

/// 沙箱审计日志（进程内 append-only）。
#[derive(Debug, Clone, Default)]
pub struct SandboxAuditLog {
    events: Vec<SandboxAuditEvent>,
}

impl SandboxAuditLog {
    pub fn record(
        &mut self,
        kind: SandboxEventKind,
        sandbox: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.events.push(SandboxAuditEvent {
            ts: Utc::now().to_rfc3339(),
            kind,
            sandbox: sandbox.into(),
            detail: detail.into(),
        });
    }

    pub fn events(&self) -> &[SandboxAuditEvent] {
        &self.events
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn contains_kind(&self, kind: SandboxEventKind) -> bool {
        self.events.iter().any(|event| event.kind == kind)
    }

    pub fn drain(&mut self) -> Vec<SandboxAuditEvent> {
        std::mem::take(&mut self.events)
    }
}

/// 沙箱管理器：策略校验 → 能力评估 → 显式降级/拒绝 → 审计，统一入口。
pub struct SandboxManager {
    executor: Box<dyn SandboxExecutor>,
    platform: PlatformSupport,
    audit: SandboxAuditLog,
}

/// 进程执行门卫结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecGuard {
    /// 可执行（若为 Degraded，降级已写审计事件）。
    Allowed { degraded: Option<IsolationLevel> },
    /// 已拒绝（审计已写），不得执行。
    Blocked,
}

/// 全局默认沙箱管理器（惰性初始化：真实探测 + Windows 执行器）。
pub fn default_manager() -> Arc<Mutex<SandboxManager>> {
    static MANAGER: OnceLock<Arc<Mutex<SandboxManager>>> = OnceLock::new();
    MANAGER
        .get_or_init(|| {
            let support = probe_platform_support();
            #[cfg(target_os = "windows")]
            let executor: Box<dyn SandboxExecutor> =
                match win::WindowsSandboxExecutor::detect(&support) {
                    Some(executor) => Box::new(executor),
                    None => Box::new(UnavailableExecutor {
                        reason: support.reason.clone(),
                    }),
                };
            #[cfg(not(target_os = "windows"))]
            let executor: Box<dyn SandboxExecutor> = Box::new(UnavailableExecutor {
                reason: support.reason.clone(),
            });
            let manager = SandboxManager::with_probe(executor);
            Arc::new(Mutex::new(manager))
        })
        .clone()
}

impl SandboxManager {
    pub fn new(executor: Box<dyn SandboxExecutor>, platform: PlatformSupport) -> Self {
        Self {
            executor,
            platform,
            audit: SandboxAuditLog::default(),
        }
    }

    /// 用平台探测结果构造管理器（探测本身也记录审计事件）。
    pub fn with_probe(executor: Box<dyn SandboxExecutor>) -> Self {
        let platform = probe_platform_support();
        let mut manager = Self::new(executor, platform.clone());
        manager.audit.record(
            SandboxEventKind::CapabilityProbe,
            "probe",
            format!(
                "os={} app_container={} job_object={} low_integrity={}；{}",
                platform.os,
                platform.app_container,
                platform.job_object,
                platform.low_integrity,
                platform.reason
            ),
        );
        manager
    }

    pub fn executor(&self) -> &dyn SandboxExecutor {
        self.executor.as_ref()
    }

    pub fn platform(&self) -> &PlatformSupport {
        &self.platform
    }

    pub fn audit(&self) -> &SandboxAuditLog {
        &self.audit
    }

    pub fn take_audit_events(&mut self) -> Vec<SandboxAuditEvent> {
        self.audit.drain()
    }

    /// 记录 egress 拒绝事件（R9：插件/工具越界网络拒绝时调用，可汇入审计链）。
    pub fn record_egress_rejection(
        &mut self,
        sandbox: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.audit
            .record(SandboxEventKind::EgressRejected, sandbox, detail);
    }

    /// 记录插件拒绝事件（R10：吊销/高风险扫描拒绝时调用，可汇入审计链）。
    pub fn record_plugin_rejection(
        &mut self,
        plugin: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.audit
            .record(SandboxEventKind::PluginRejected, plugin, detail);
    }

    /// 沙箱审计事件汇入 HMAC 审计链（R8：凭据与审计闭环）。
    pub fn drain_into_chain(
        &mut self,
        chain: &mut crate::audit_chain::AuditChain,
        actor: &str,
    ) -> usize {
        let mut log = SandboxAuditLog::default();
        for event in self.audit.drain() {
            log.record(event.kind, event.sandbox, event.detail);
        }
        chain.append_sandbox_log(&log, actor)
    }

    /// 门卫：策略校验 + 能力评估 + 审计（不实际启动进程）。
    pub fn guard(&mut self, command: &SandboxCommand) -> Result<ExecGuard, SandboxError> {
        if let Err(error) = command.validate() {
            self.audit.record(
                SandboxEventKind::SpawnRejected,
                &command.policy.name,
                error.to_string(),
            );
            return Err(error);
        }
        match evaluate_capability(&self.platform, &command.policy) {
            Err(error) => {
                self.audit.record(
                    SandboxEventKind::UnsupportedIsolation,
                    &command.policy.name,
                    error.to_string(),
                );
                Err(error)
            }
            Ok(CapabilityEvaluation::Degraded(level)) => {
                self.audit.record(
                    SandboxEventKind::DegradedIsolation,
                    &command.policy.name,
                    format!("显式降级到 {:?}", level),
                );
                Ok(ExecGuard::Allowed {
                    degraded: Some(level),
                })
            }
            Ok(CapabilityEvaluation::Full) => Ok(ExecGuard::Allowed { degraded: None }),
        }
    }

    /// 经沙箱执行命令：门卫 + executor.spawn。
    pub fn spawn(&mut self, command: &SandboxCommand) -> Result<SandboxProcess, SandboxError> {
        self.guard(command)?;
        match self.executor.spawn(command) {
            Ok(process) => Ok(process),
            Err(error) => {
                self.audit.record(
                    SandboxEventKind::SpawnRejected,
                    &command.policy.name,
                    error.to_string(),
                );
                Err(error)
            }
        }
    }

    /// 把已启动的进程（如 MCP 子进程）挂入受限 Job；失败 = 显式拒绝。
    /// 网络策略（R10）：AllowList/Unrestricted 需要 OS 级网络强制（AppContainer），
    /// Job 挂接路径无法强制网络 → 显式拒绝（不静默放开）。
    pub fn attach_pid(
        &mut self,
        policy: &SandboxPolicy,
        pid: u32,
    ) -> Result<JobGuard, SandboxError> {
        policy.validate()?;
        if network_requires_app_container(policy) {
            return Err(SandboxError::Unsupported(format!(
                "网络策略 {:?} 需要 AppContainer 隔离才能强制网络白名单，\
                 Job 挂接路径无法强制（显式拒绝，不静默放开网络）",
                policy.network_policy
            )));
        }
        let guard = self.executor.attach(policy, pid)?;
        self.audit.record(
            SandboxEventKind::Attached,
            &policy.name,
            format!("进程 {} 已挂入受限 Job", pid),
        );
        Ok(guard)
    }

    pub fn kill(&mut self, handle: &SandboxHandle) -> Result<(), SandboxError> {
        match self.executor.kill(handle) {
            Ok(()) => {
                self.audit
                    .record(SandboxEventKind::Killed, &handle.id, "沙箱进程已终止");
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    pub fn check_healthy(&mut self) -> SandboxHealth {
        let health = self.executor.check_healthy();
        if !health.healthy {
            self.audit.record(
                SandboxEventKind::Unhealthy,
                "executor",
                health.detail.clone(),
            );
        }
        health
    }
}

/// 测试/本地用执行器：可编程的 spawn/kill/健康结果。
#[derive(Debug)]
pub struct MockSandboxExecutor {
    pub name: &'static str,
    pub isolation: IsolationLevel,
    pub healthy: std::sync::atomic::AtomicBool,
    pub spawn_should_fail: std::sync::atomic::AtomicBool,
    pub kill_should_fail: std::sync::atomic::AtomicBool,
    pub spawn_calls: std::sync::atomic::AtomicUsize,
    pub kill_calls: std::sync::atomic::AtomicUsize,
}

impl Default for MockSandboxExecutor {
    fn default() -> Self {
        Self {
            name: "mock",
            isolation: IsolationLevel::AppContainerJob,
            healthy: std::sync::atomic::AtomicBool::new(true),
            spawn_should_fail: std::sync::atomic::AtomicBool::new(false),
            kill_should_fail: std::sync::atomic::AtomicBool::new(false),
            spawn_calls: std::sync::atomic::AtomicUsize::new(0),
            kill_calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl MockSandboxExecutor {
    pub fn with_isolation(isolation: IsolationLevel) -> Self {
        Self {
            isolation,
            ..MockSandboxExecutor::default()
        }
    }
}

impl SandboxExecutor for MockSandboxExecutor {
    fn name(&self) -> &'static str {
        self.name
    }

    fn capability(&self) -> IsolationLevel {
        self.isolation
    }

    fn spawn(&self, command: &SandboxCommand) -> Result<SandboxProcess, SandboxError> {
        self.spawn_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self
            .spawn_should_fail
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(SandboxError::Spawn("mock 执行器启动失败".to_string()));
        }
        Ok(SandboxProcess {
            handle: SandboxHandle {
                id: format!("mock-{}", command.policy.name),
                spawned_at: Utc::now().to_rfc3339(),
            },
            status: SandboxProcessStatus::Running,
            stdout: Vec::new(),
            stderr: Vec::new(),
            inner: None,
        })
    }

    fn kill(&self, _handle: &SandboxHandle) -> Result<(), SandboxError> {
        self.kill_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self
            .kill_should_fail
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(SandboxError::Kill("mock 执行器终止失败".to_string()));
        }
        Ok(())
    }

    fn check_healthy(&self) -> SandboxHealth {
        SandboxHealth {
            healthy: self.healthy.load(std::sync::atomic::Ordering::SeqCst),
            detail: "mock 健康检查".to_string(),
        }
    }
}

/// 显式不可用的执行器：任何操作返回 Unsupported（禁止假装安全）。
pub struct UnavailableExecutor {
    pub reason: String,
}

impl SandboxExecutor for UnavailableExecutor {
    fn name(&self) -> &'static str {
        "unavailable"
    }

    fn capability(&self) -> IsolationLevel {
        IsolationLevel::None
    }

    fn spawn(&self, _command: &SandboxCommand) -> Result<SandboxProcess, SandboxError> {
        Err(SandboxError::Unsupported(self.reason.clone()))
    }

    fn kill(&self, _handle: &SandboxHandle) -> Result<(), SandboxError> {
        Err(SandboxError::Unsupported(self.reason.clone()))
    }

    fn check_healthy(&self) -> SandboxHealth {
        SandboxHealth {
            healthy: false,
            detail: self.reason.clone(),
        }
    }
}

/// Job 守卫：持有 Job Object 句柄；Drop 时终止 job 内全部进程（防孤儿）。
pub struct JobGuard {
    pub pid: u32,
    #[cfg(target_os = "windows")]
    job: win::Handle,
}

impl std::fmt::Debug for JobGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JobGuard")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

// 句柄值可跨线程转移（由 JobGuard 独占管理），标准 Windows 实践。
#[cfg(target_os = "windows")]
unsafe impl Send for JobGuard {}

#[cfg(target_os = "windows")]
impl Drop for JobGuard {
    fn drop(&mut self) {
        win::terminate_job(self.job, 1);
        win::close_handle(self.job);
    }
}

/// 路径工具：判断目标是否位于工作区内（越界样例矩阵用）。
pub fn inside_workspace(workspace: &Path, target: &Path) -> bool {
    target.starts_with(workspace)
}

/// AppContainer 网络能力 SID（S-1-15-2-1 InternetClient）。
pub fn internet_client_sid() -> Vec<u8> {
    app_package_sid(1)
}

/// AppContainer 网络能力 SID（S-1-15-2-3 PrivateNetworkClientServer）。
pub fn private_network_client_server_sid() -> Vec<u8> {
    app_package_sid(3)
}

/// 构造 S-1-15-2-<rid> 包能力 SID 字节（revision=1, count=2, authority=15, subauth=[2, rid]）。
fn app_package_sid(rid: u32) -> Vec<u8> {
    let mut sid = vec![1u8, 2u8, 0, 0, 0, 0, 0, 15];
    sid.extend_from_slice(&2u32.to_le_bytes());
    sid.extend_from_slice(&rid.to_le_bytes());
    sid
}

/// host 是否为内网（私有 IP 段 / 无点 netbios 名）。host 可含 :port。
fn is_private_host(host: &str) -> bool {
    let hostname = host.split(':').next().unwrap_or(host);
    if hostname.starts_with("127.")
        || hostname.starts_with("10.")
        || hostname.starts_with("192.168.")
        || hostname.starts_with("169.254.")
        || hostname.starts_with("172.16.")
        || hostname.starts_with("172.17.")
        || hostname.starts_with("172.18.")
        || hostname.starts_with("172.19.")
        || hostname.starts_with("172.2")
        || hostname.starts_with("172.30.")
        || hostname.starts_with("172.31.")
    {
        return true;
    }
    !hostname.contains('.')
}

/// 按网络策略生成 AppContainer 网络能力 SID 列表：
/// - `None`/`Loopback` → 空（默认 deny；AppContainer 下 loopback 需 OS 级豁免，Wave 3 接入）；
/// - `AllowList` → 按 host 推断（内网 → PrivateNetworkClientServer，公网 → InternetClient）；
/// - `Unrestricted` → InternetClient + PrivateNetworkClientServer（需显式放行，policy.validate 把关）。
pub fn app_container_network_capabilities(policy: &SandboxPolicy) -> Vec<Vec<u8>> {
    match policy.network_policy {
        NetworkPolicy::None | NetworkPolicy::Loopback => Vec::new(),
        NetworkPolicy::Unrestricted => {
            vec![internet_client_sid(), private_network_client_server_sid()]
        }
        NetworkPolicy::AllowList => {
            let mut sids: Vec<Vec<u8>> = Vec::new();
            for host in &policy.allow_hosts {
                let sid = if is_private_host(host) {
                    private_network_client_server_sid()
                } else {
                    internet_client_sid()
                };
                if !sids.contains(&sid) {
                    sids.push(sid);
                }
            }
            sids
        }
    }
}

/// 校验生成的网络能力与策略一致（默认 deny 的隔离策略不得携带网络能力）。
pub fn validate_app_container_network(
    policy: &SandboxPolicy,
    sids: &[Vec<u8>],
) -> Result<(), SandboxError> {
    match policy.network_policy {
        NetworkPolicy::None | NetworkPolicy::Loopback => {
            if !sids.is_empty() {
                return Err(SandboxError::PolicyViolation(
                    "隔离网络策略下 AppContainer 不得携带网络能力".to_string(),
                ));
            }
        }
        NetworkPolicy::AllowList => {
            if sids.is_empty() {
                return Err(SandboxError::PolicyViolation(
                    "AllowList 网络策略要求网络能力但生成为空".to_string(),
                ));
            }
        }
        NetworkPolicy::Unrestricted => {
            if !sids.iter().any(|sid| *sid == internet_client_sid()) {
                return Err(SandboxError::PolicyViolation(
                    "Unrestricted 网络策略必须携带 InternetClient 能力".to_string(),
                ));
            }
        }
    }
    Ok(())
}

/// 网络 egress 边界（R9）：`AllowList`/`Unrestricted` 网络策略必须在 OS 边界强制
/// （AppContainer 网络能力），仅 Job/LowIL 隔离无法限制网络 → 调用方必须显式拒绝。
pub fn network_requires_app_container(policy: &SandboxPolicy) -> bool {
    matches!(
        policy.network_policy,
        NetworkPolicy::AllowList | NetworkPolicy::Unrestricted
    )
}

/// Windows 结构布局与 SDK 一致性检查（防 ABI 漂移；非 Windows 恒真）。
pub fn os_struct_layouts_match() -> bool {
    #[cfg(target_os = "windows")]
    {
        win::assert_struct_layouts()
    }
    #[cfg(not(target_os = "windows"))]
    {
        true
    }
}

#[cfg(target_os = "windows")]
pub(crate) mod win {
    //! Windows raw FFI 层：Job Object / 令牌 / AppContainer / 管道。
    //! 全部 API 为系统自带导出（kernel32/advapi32/ntdll），**不引入新依赖**。
    //! 结构布局与 Windows SDK 保持一致（repr(C)，测试含尺寸断言）。
    #![allow(dead_code)]
    #![allow(non_camel_case_types)]
    #![allow(clippy::upper_case_acronyms)]

    use super::*;
    use std::ffi::c_void;
    use std::io::Read;
    use std::os::raw::c_char;

    pub type BOOL = i32;
    pub type DWORD = u32;
    pub type Handle = *mut c_void;
    pub type SIZE_T = usize;
    pub type ULONG_PTR = usize;
    pub type PSID = *mut c_void;

    pub const TRUE: BOOL = 1;
    pub const FALSE: BOOL = 0;
    pub const INFINITE: DWORD = 0xFFFF_FFFF;
    pub const STILL_ACTIVE: DWORD = 259;
    pub const ERROR_NOT_FOUND: DWORD = 1168;

    // Job Object
    pub const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: DWORD = 0x0000_2000;
    pub const JOB_OBJECT_LIMIT_ACTIVE_PROCESS: DWORD = 0x0000_0008;
    pub const JOB_OBJECT_LIMIT_JOB_MEMORY: DWORD = 0x0000_0200;
    pub const JOB_OBJECT_LIMIT_JOB_TIME: DWORD = 0x0000_0004;
    pub const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;

    // 令牌
    pub const TOKEN_DUPLICATE: DWORD = 0x0002;
    pub const TOKEN_QUERY: DWORD = 0x0008;
    pub const MAXIMUM_ALLOWED: DWORD = 0x0200_0000;
    pub const TOKEN_PRIMARY: DWORD = 1;
    pub const SECURITY_IMPERSONATION: DWORD = 2;
    pub const TOKEN_INTEGRITY_LEVEL: DWORD = 25;
    pub const SYSTEM_MANDATORY_LABEL_ACE_TYPE: u8 = 0x11;
    pub const SECURITY_MANDATORY_LOW_RID: DWORD = 0x1000;

    // 进程/线程属性（AppContainer）
    pub const EXTENDED_STARTUPINFO_PRESENT: DWORD = 0x0008_0000;
    pub const PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES: ULONG_PTR = 0x0002_0009;

    // 访问权限
    pub const PROCESS_ALL_ACCESS: DWORD = 0x001F_0FFF;
    pub const PROCESS_QUERY_INFORMATION: DWORD = 0x0400;

    // Credential Manager
    pub const CRED_TYPE_GENERIC: DWORD = 1;
    pub const CRED_PERSIST_LOCAL_MACHINE: DWORD = 2;

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateJobObjectW(lp_job_attributes: *const c_void, lp_name: *const u16) -> Handle;
        fn SetInformationJobObject(
            h_job: Handle,
            job_object_information_class: i32,
            lp_job_object_information: *const c_void,
            cb_job_object_information_length: DWORD,
        ) -> BOOL;
        fn AssignProcessToJobObject(h_job: Handle, h_process: Handle) -> BOOL;
        fn TerminateJobObject(h_job: Handle, u_exit_code: u32) -> BOOL;
        fn OpenProcess(
            dw_desired_access: DWORD,
            b_inherit_handle: BOOL,
            dw_process_id: DWORD,
        ) -> Handle;
        fn GetCurrentProcess() -> Handle;
        fn CloseHandle(h_object: Handle) -> BOOL;
        fn CreatePipe(
            h_read_pipe: *mut Handle,
            h_write_pipe: *mut Handle,
            lp_pipe_attributes: *const c_void,
            n_size: DWORD,
        ) -> BOOL;
        fn CreateProcessW(
            lp_application_name: *const u16,
            lp_command_line: *mut u16,
            lp_process_attributes: *const c_void,
            lp_thread_attributes: *const c_void,
            b_inherit_handles: BOOL,
            dw_creation_flags: DWORD,
            lp_environment: *const c_void,
            lp_current_directory: *const u16,
            lp_startup_info: *mut c_void,
            lp_process_information: *mut PROCESS_INFORMATION,
        ) -> BOOL;
        fn InitializeProcThreadAttributeList(
            lp_attribute_list: *mut c_void,
            dw_attribute_count: DWORD,
            dw_flags: DWORD,
            lp_size: *mut SIZE_T,
        ) -> BOOL;
        fn UpdateProcThreadAttribute(
            lp_attribute_list: *mut c_void,
            dw_flags: DWORD,
            attribute: ULONG_PTR,
            lp_value: *const c_void,
            cb_size: SIZE_T,
            lp_previous_value: *mut c_void,
            lp_return_size: *mut SIZE_T,
        ) -> BOOL;
        fn DeleteProcThreadAttributeList(lp_attribute_list: *mut c_void);
        fn ReadFile(
            h_file: Handle,
            lp_buffer: *mut u8,
            n_number_of_bytes_to_read: DWORD,
            lp_number_of_bytes_read: *mut DWORD,
            lp_overlapped: *mut c_void,
        ) -> BOOL;
        fn WaitForSingleObject(h_handle: Handle, dw_milliseconds: DWORD) -> DWORD;
        fn GetExitCodeProcess(h_process: Handle, lp_exit_code: *mut DWORD) -> BOOL;
        fn GetLastError() -> DWORD;
        fn LoadLibraryW(lp_file_name: *const u16) -> Handle;
        fn GetProcAddress(h_module: Handle, lp_proc_name: *const c_char) -> *mut c_void;
        fn TerminateProcess(h_process: Handle, u_exit_code: u32) -> BOOL;
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn OpenProcessToken(
            process_handle: Handle,
            desired_access: DWORD,
            token_handle: *mut Handle,
        ) -> BOOL;
        fn DuplicateTokenEx(
            existing_token_handle: Handle,
            desired_access: DWORD,
            token_attributes: *const c_void,
            impersonation_level: DWORD,
            token_type: DWORD,
            new_token_handle: *mut Handle,
        ) -> BOOL;
        fn SetTokenInformation(
            token_handle: Handle,
            token_information_class: DWORD,
            token_information: *const c_void,
            token_information_length: DWORD,
        ) -> BOOL;
        fn CreateProcessAsUserW(
            h_token: Handle,
            lp_application_name: *const u16,
            lp_command_line: *mut u16,
            lp_process_attributes: *const c_void,
            lp_thread_attributes: *const c_void,
            b_inherit_handles: BOOL,
            dw_creation_flags: DWORD,
            lp_environment: *const c_void,
            lp_current_directory: *const u16,
            lp_startup_info: *mut c_void,
            lp_process_information: *mut PROCESS_INFORMATION,
        ) -> BOOL;
        fn DeriveAppContainerSidFromAppContainerName(
            psz_app_container_name: *const u16,
            psid: *mut PSID,
        ) -> BOOL;
        fn FreeSid(psid: PSID);
        fn CredWriteW(credential: *const CREDENTIALW, flags: DWORD) -> BOOL;
        fn CredReadW(
            target_name: *mut u16,
            typ: DWORD,
            flags: DWORD,
            credential: *mut *mut CREDENTIALW,
        ) -> BOOL;
        fn CredDeleteW(target_name: *mut u16, typ: DWORD, flags: DWORD) -> BOOL;
        fn CredFree(buffer: *mut c_void);
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn RtlGetVersion(lp_version_information: *mut OSVERSIONINFOW) -> i32;
    }

    #[repr(C)]
    pub struct OSVERSIONINFOW {
        pub dw_os_version_info_size: DWORD,
        pub dw_major_version: DWORD,
        pub dw_minor_version: DWORD,
        pub dw_build_number: DWORD,
        pub dw_platform_id: DWORD,
        pub sz_csd_version: [u16; 128],
    }

    #[repr(C)]
    pub struct JOBOBJECT_BASIC_LIMIT_INFORMATION {
        pub per_process_user_time_limit: i64,
        pub per_job_user_time_limit: i64,
        pub limit_flags: DWORD,
        pub minimum_working_set_size: SIZE_T,
        pub maximum_working_set_size: SIZE_T,
        pub active_process_limit: DWORD,
        pub affinity: ULONG_PTR,
        pub priority_class: DWORD,
        pub scheduling_class: DWORD,
    }

    #[repr(C)]
    pub struct JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
        pub basic_limit_information: JOBOBJECT_BASIC_LIMIT_INFORMATION,
        pub io_info: [u64; 6],
        pub process_memory_limit: SIZE_T,
        pub job_memory_limit: SIZE_T,
        pub peak_process_memory_used: SIZE_T,
        pub peak_job_memory_used: SIZE_T,
    }

    #[repr(C)]
    pub struct PROCESS_INFORMATION {
        pub h_process: Handle,
        pub h_thread: Handle,
        pub dw_process_id: DWORD,
        pub dw_thread_id: DWORD,
    }

    #[repr(C)]
    pub struct STARTUPINFOW {
        pub cb: DWORD,
        pub lp_reserved: *mut u16,
        pub lp_desktop: *mut u16,
        pub lp_title: *mut u16,
        pub dw_x: DWORD,
        pub dw_y: DWORD,
        pub dw_x_size: DWORD,
        pub dw_y_size: DWORD,
        pub dw_x_count_chars: DWORD,
        pub dw_y_count_chars: DWORD,
        pub dw_fill_attribute: DWORD,
        pub dw_flags: DWORD,
        pub w_show_window: u16,
        pub cb_reserved2: u16,
        pub lp_reserved2: *mut u8,
        pub h_std_input: Handle,
        pub h_std_output: Handle,
        pub h_std_error: Handle,
    }

    #[repr(C)]
    pub struct STARTUPINFOEXW {
        pub startup_info: STARTUPINFOW,
        pub lp_attribute_list: *mut c_void,
    }

    #[repr(C)]
    pub struct SID_AND_ATTRIBUTES {
        pub sid: PSID,
        pub attributes: DWORD,
    }

    #[repr(C)]
    pub struct SECURITY_CAPABILITIES {
        pub app_container_sid: PSID,
        pub capabilities: *mut SID_AND_ATTRIBUTES,
        pub capability_count: DWORD,
        pub reserved: DWORD,
    }

    #[repr(C)]
    pub struct ACE_HEADER {
        pub ace_type: u8,
        pub ace_flags: u8,
        pub ace_size: u16,
    }

    #[repr(C)]
    pub struct SID_MINIMAL {
        pub revision: u8,
        pub sub_authority_count: u8,
        pub identifier_authority: [u8; 6],
        pub sub_authority: [DWORD; 1],
    }

    #[repr(C)]
    pub struct SYSTEM_MANDATORY_LABEL_ACE {
        pub header: ACE_HEADER,
        pub mask: DWORD,
        pub sid_start: SID_MINIMAL,
    }

    #[repr(C)]
    pub struct CREDENTIALW {
        pub flags: DWORD,
        pub cred_type: DWORD,
        pub target_name: *mut u16,
        pub comment: *mut u16,
        pub last_written: [DWORD; 2],
        pub credential_blob_size: DWORD,
        pub credential_blob: *mut u8,
        pub persist: DWORD,
        pub attribute_count: DWORD,
        pub attributes: *mut c_void,
        pub target_alias: *mut u16,
        pub user_name: *mut u16,
    }

    #[repr(C)]
    pub struct SECURITY_ATTRIBUTES {
        pub n_length: DWORD,
        pub lp_security_descriptor: *mut c_void,
        pub b_inherit_handle: BOOL,
    }

    pub fn to_wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn last_error() -> DWORD {
        unsafe { GetLastError() }
    }

    pub fn close_handle(handle: Handle) {
        if !handle.is_null() {
            unsafe {
                CloseHandle(handle);
            }
        }
    }

    pub fn terminate_job(job: Handle, exit_code: u32) {
        if !job.is_null() {
            unsafe {
                TerminateJobObject(job, exit_code);
            }
        }
    }

    /// 创建受限 Job（kill-on-close + 资源上限）。返回句柄，失败返回 None。
    pub fn create_job(policy: &SandboxPolicy) -> Option<Handle> {
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return None;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        let mut flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if let Some(limit) = policy.active_process_limit {
            flags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            info.basic_limit_information.active_process_limit = limit;
        }
        if let Some(mem_mb) = policy.mem_mb {
            flags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
            info.job_memory_limit = (mem_mb as usize).saturating_mul(1024 * 1024);
        }
        if let Some(cpu_ms) = policy.cpu_ms {
            flags |= JOB_OBJECT_LIMIT_JOB_TIME;
            // 100ns 单位
            info.basic_limit_information.per_job_user_time_limit =
                (cpu_ms as i64).saturating_mul(10_000);
        }
        info.basic_limit_information.limit_flags = flags;
        let ok = unsafe {
            SetInformationJobObject(
                job,
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                &info as *const _ as *const c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as DWORD,
            )
        };
        if ok == FALSE {
            close_handle(job);
            return None;
        }
        Some(job)
    }

    /// 把 PID 进程挂入 Job。
    pub fn assign_pid_to_job(job: Handle, pid: u32) -> bool {
        let process = unsafe { OpenProcess(PROCESS_ALL_ACCESS, FALSE, pid) };
        if process.is_null() {
            return false;
        }
        let ok = unsafe { AssignProcessToJobObject(job, process) };
        close_handle(process);
        ok == TRUE
    }

    /// OS 版本探测（RtlGetVersion；失败按保守处理）。
    pub fn os_version() -> (u32, u32) {
        let mut info: OSVERSIONINFOW = unsafe { std::mem::zeroed() };
        info.dw_os_version_info_size = std::mem::size_of::<OSVERSIONINFOW>() as DWORD;
        let status = unsafe { RtlGetVersion(&mut info) };
        if status != 0 {
            return (0, 0);
        }
        (info.dw_major_version, info.dw_minor_version)
    }

    /// AppContainer API 是否存在（Win8+；动态解析避免旧系统加载失败）。
    fn app_container_api_present() -> bool {
        let advapi = unsafe { LoadLibraryW(to_wide("advapi32.dll").as_ptr()) };
        if advapi.is_null() {
            return false;
        }
        let proc = unsafe {
            GetProcAddress(
                advapi,
                c"DeriveAppContainerSidFromAppContainerName".as_ptr(),
            )
        };
        // advapi32 恒驻留，无需 FreeLibrary。
        !proc.is_null()
    }

    /// 探测 AppContainer：API 存在 + 派生 SID 成功。
    pub fn probe_app_container() -> (bool, String) {
        if !app_container_api_present() {
            return (
                false,
                "AppContainer API 不可用（需要 Windows 8+）".to_string(),
            );
        }
        let name = to_wide("owo-agent-cap-probe");
        let mut sid: PSID = std::ptr::null_mut();
        let ok = unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) };
        if ok == TRUE && !sid.is_null() {
            unsafe {
                FreeSid(sid);
            }
            return (true, "AppContainer API 可用".to_string());
        }
        (
            false,
            format!("AppContainer SID 派生失败（错误 {}）", last_error()),
        )
    }

    /// 低完整性标签（20 字节 SYSTEM_MANDATORY_LABEL_ACE）。
    fn low_integrity_label() -> SYSTEM_MANDATORY_LABEL_ACE {
        SYSTEM_MANDATORY_LABEL_ACE {
            header: ACE_HEADER {
                ace_type: SYSTEM_MANDATORY_LABEL_ACE_TYPE,
                ace_flags: 0,
                ace_size: std::mem::size_of::<SYSTEM_MANDATORY_LABEL_ACE>() as u16,
            },
            mask: 0,
            sid_start: SID_MINIMAL {
                revision: 1,
                sub_authority_count: 1,
                identifier_authority: [0, 0, 0, 0, 0, 16],
                sub_authority: [SECURITY_MANDATORY_LOW_RID],
            },
        }
    }

    /// 探测低完整性令牌：复制当前令牌并设置 Low IL（不改动当前令牌，安全）。
    pub fn probe_low_integrity() -> (bool, String) {
        let current = unsafe { GetCurrentProcess() };
        let mut token: Handle = std::ptr::null_mut();
        if unsafe { OpenProcessToken(current, TOKEN_DUPLICATE, &mut token) } == FALSE {
            return (
                false,
                format!("OpenProcessToken 失败（错误 {}）", last_error()),
            );
        }
        let mut duplicate: Handle = std::ptr::null_mut();
        let dup_ok = unsafe {
            DuplicateTokenEx(
                token,
                MAXIMUM_ALLOWED,
                std::ptr::null(),
                SECURITY_IMPERSONATION,
                TOKEN_PRIMARY,
                &mut duplicate,
            )
        };
        close_handle(token);
        if dup_ok == FALSE || duplicate.is_null() {
            return (
                false,
                format!("DuplicateTokenEx 失败（错误 {}）", last_error()),
            );
        }
        let label = low_integrity_label();
        let set_ok = unsafe {
            SetTokenInformation(
                duplicate,
                TOKEN_INTEGRITY_LEVEL,
                &label as *const SYSTEM_MANDATORY_LABEL_ACE as *const c_void,
                std::mem::size_of::<SYSTEM_MANDATORY_LABEL_ACE>() as DWORD,
            )
        };
        close_handle(duplicate);
        if set_ok == FALSE {
            return (
                false,
                format!("SetTokenInformation(Low IL) 失败（错误 {}）", last_error()),
            );
        }
        (true, "低完整性令牌可用".to_string())
    }

    /// Windows 真实能力探测。
    pub fn probe_windows_support() -> PlatformSupport {
        let (major, minor) = os_version();
        let version_note = format!("Windows {major}.{minor}");
        let mut reasons = Vec::new();

        let job = create_job(&SandboxPolicy::default());
        let job_object = job.is_some();
        if let Some(job) = job {
            terminate_job(job, 1);
            close_handle(job);
        }
        reasons.push(if job_object {
            "Job Object 创建成功".to_string()
        } else {
            format!("Job Object 不可用（错误 {}）", last_error())
        });

        let (low_integrity, low_reason) = probe_low_integrity();
        reasons.push(if low_integrity {
            "低完整性令牌可用".to_string()
        } else {
            low_reason
        });

        let (app_container, ac_reason) = probe_app_container();
        reasons.push(if app_container {
            "AppContainer 可用".to_string()
        } else {
            ac_reason
        });

        PlatformSupport {
            os: "windows".to_string(),
            app_container,
            job_object,
            low_integrity,
            reason: format!("{}；{}", version_note, reasons.join("；")),
        }
    }

    /// Windows 沙箱执行器：Job 基线 + LowIL/AppContainer 按策略升级。
    pub struct WindowsSandboxExecutor {
        support: PlatformSupport,
    }

    impl WindowsSandboxExecutor {
        pub fn detect(support: &PlatformSupport) -> Option<Self> {
            if !support.job_object {
                return None;
            }
            Some(Self {
                support: support.clone(),
            })
        }

        /// 按策略选择隔离创建方式：AppContainer → LowIL → Job-only。
        fn create_process(
            &self,
            command: &SandboxCommand,
            job: Handle,
        ) -> Result<OsChild, SandboxError> {
            let required = command.policy.require_isolation;
            if required >= IsolationLevel::AppContainerJob && self.support.app_container {
                return self.spawn_app_container(command, job);
            }
            if required >= IsolationLevel::LowIntegrity && self.support.low_integrity {
                return self.spawn_low_integrity(command, job);
            }
            self.spawn_plain(command, job)
        }

        /// 普通路径：std::process::Command（可靠 quoting）+ Job 挂接。
        fn spawn_plain(
            &self,
            command: &SandboxCommand,
            job: Handle,
        ) -> Result<OsChild, SandboxError> {
            let mut cmd = std::process::Command::new(&command.program);
            cmd.args(&command.args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            if let Some(cwd) = &command.cwd {
                cmd.current_dir(cwd);
            }
            for (key, value) in &command.env {
                cmd.env(key, value);
            }
            let mut child = cmd
                .spawn()
                .map_err(|error| SandboxError::Spawn(format!("{}：{error}", command.program)))?;
            let pid = child.id();
            if !assign_pid_to_job(job, pid) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(SandboxError::Spawn(format!(
                    "进程 {pid} 无法挂入 Job（错误 {}），已终止",
                    last_error()
                )));
            }
            Ok(OsChild::StdChild { child })
        }

        /// 低完整性路径：受限令牌 + Low IL 标签 + CreateProcessAsUserW。
        fn spawn_low_integrity(
            &self,
            command: &SandboxCommand,
            job: Handle,
        ) -> Result<OsChild, SandboxError> {
            let current = unsafe { GetCurrentProcess() };
            let mut token: Handle = std::ptr::null_mut();
            if unsafe { OpenProcessToken(current, TOKEN_DUPLICATE, &mut token) } == FALSE {
                return Err(SandboxError::Spawn(format!(
                    "OpenProcessToken 失败（错误 {}）",
                    last_error()
                )));
            }
            let mut primary: Handle = std::ptr::null_mut();
            let dup_ok = unsafe {
                DuplicateTokenEx(
                    token,
                    MAXIMUM_ALLOWED,
                    std::ptr::null(),
                    SECURITY_IMPERSONATION,
                    TOKEN_PRIMARY,
                    &mut primary,
                )
            };
            close_handle(token);
            if dup_ok == FALSE || primary.is_null() {
                return Err(SandboxError::Spawn(format!(
                    "DuplicateTokenEx 失败（错误 {}）",
                    last_error()
                )));
            }
            let label = low_integrity_label();
            let set_ok = unsafe {
                SetTokenInformation(
                    primary,
                    TOKEN_INTEGRITY_LEVEL,
                    &label as *const SYSTEM_MANDATORY_LABEL_ACE as *const c_void,
                    std::mem::size_of::<SYSTEM_MANDATORY_LABEL_ACE>() as DWORD,
                )
            };
            if set_ok == FALSE {
                close_handle(primary);
                return Err(SandboxError::Spawn(format!(
                    "SetTokenInformation(Low IL) 失败（错误 {}）",
                    last_error()
                )));
            }
            let (pi, pipes) = create_process_with_token(command, job, |startup, cmdline, pi| {
                let result = unsafe {
                    CreateProcessAsUserW(
                        primary,
                        std::ptr::null(),
                        cmdline,
                        std::ptr::null(),
                        std::ptr::null(),
                        TRUE,
                        0,
                        std::ptr::null(),
                        std::ptr::null(),
                        &mut startup.startup_info as *mut STARTUPINFOW as *mut c_void,
                        pi,
                    )
                };
                if result == FALSE {
                    Err(SandboxError::Spawn(format!(
                        "CreateProcessAsUserW(Low IL) 失败（错误 {}）",
                        last_error()
                    )))
                } else {
                    Ok(())
                }
            })?;
            close_handle(primary);
            Ok(OsChild::OsChild {
                pi,
                stdout_read: pipes.read_stdout,
                stderr_read: pipes.read_stderr,
            })
        }

        /// AppContainer 路径：SECURITY_CAPABILITIES 属性 + CreateProcessW。
        fn spawn_app_container(
            &self,
            command: &SandboxCommand,
            job: Handle,
        ) -> Result<OsChild, SandboxError> {
            let name = to_wide("owo-agent-container");
            let mut sid: PSID = std::ptr::null_mut();
            if unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) }
                == FALSE
                || sid.is_null()
            {
                return Err(SandboxError::Spawn(format!(
                    "AppContainer SID 派生失败（错误 {}）",
                    last_error()
                )));
            }
            // 网络能力白名单：按策略生成 SID 并校验（隔离策略不得带网络能力）。
            let capability_sids = app_container_network_capabilities(&command.policy);
            validate_app_container_network(&command.policy, &capability_sids)?;
            let sid_boxes: Vec<Box<[u8]>> = capability_sids
                .iter()
                .map(|sid| sid.clone().into_boxed_slice())
                .collect();
            let attrs: Vec<SID_AND_ATTRIBUTES> = sid_boxes
                .iter()
                .map(|boxed| SID_AND_ATTRIBUTES {
                    sid: boxed.as_ptr() as PSID,
                    attributes: 0,
                })
                .collect();
            let capabilities = SECURITY_CAPABILITIES {
                app_container_sid: sid,
                capabilities: attrs.as_ptr() as *mut SID_AND_ATTRIBUTES,
                capability_count: attrs.len() as DWORD,
                reserved: 0,
            };
            let (pi, pipes) =
                create_process_with_token(command, job, move |startup, cmdline, pi| {
                    let mut size: SIZE_T = 0;
                    let size_ok = unsafe {
                        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size)
                    };
                    if size_ok == FALSE && size == 0 {
                        return Err(SandboxError::Spawn(
                            "InitializeProcThreadAttributeList 尺寸获取失败".to_string(),
                        ));
                    }
                    let mut buffer = vec![0u8; size];
                    let list = buffer.as_mut_ptr() as *mut c_void;
                    if unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) } == FALSE
                    {
                        return Err(SandboxError::Spawn(format!(
                            "InitializeProcThreadAttributeList 失败（错误 {}）",
                            last_error()
                        )));
                    }
                    let updated = unsafe {
                        UpdateProcThreadAttribute(
                            list,
                            0,
                            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
                            &capabilities as *const SECURITY_CAPABILITIES as *const c_void,
                            std::mem::size_of::<SECURITY_CAPABILITIES>(),
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                        )
                    };
                    if updated == FALSE {
                        unsafe {
                            DeleteProcThreadAttributeList(list);
                        }
                        return Err(SandboxError::Spawn(format!(
                            "UpdateProcThreadAttribute 失败（错误 {}）",
                            last_error()
                        )));
                    }
                    let result = unsafe {
                        CreateProcessW(
                            std::ptr::null(),
                            cmdline,
                            std::ptr::null(),
                            std::ptr::null(),
                            TRUE,
                            0,
                            std::ptr::null(),
                            std::ptr::null(),
                            &mut startup.startup_info as *mut STARTUPINFOW as *mut c_void,
                            pi,
                        )
                    };
                    // 属性列表仅需存活到 CreateProcessW 返回。
                    unsafe {
                        DeleteProcThreadAttributeList(list);
                    }
                    if result == FALSE {
                        Err(SandboxError::Spawn(format!(
                            "CreateProcessW(AppContainer) 失败（错误 {}）",
                            last_error()
                        )))
                    } else {
                        Ok(())
                    }
                })?;
            unsafe {
                FreeSid(sid);
            }
            Ok(OsChild::OsChild {
                pi,
                stdout_read: pipes.read_stdout,
                stderr_read: pipes.read_stderr,
            })
        }
    }

    /// 统一进程创建：管道 + STARTUPINFOEX + Job 挂接。
    /// `create` 闭包负责调用 CreateProcessW 族并填充 `PROCESS_INFORMATION`。
    fn create_process_with_token<F>(
        command: &SandboxCommand,
        job: Handle,
        create: F,
    ) -> Result<(PROCESS_INFORMATION, PipePair), SandboxError>
    where
        F: FnOnce(
            &mut STARTUPINFOEXW,
            *mut u16,
            *mut PROCESS_INFORMATION,
        ) -> Result<(), SandboxError>,
    {
        let pipes = PipePair::create()
            .map_err(|error| SandboxError::Spawn(format!("CreatePipe 失败：{}", error)))?;
        let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        startup.startup_info.cb = std::mem::size_of::<STARTUPINFOEXW>() as DWORD;
        startup.startup_info.dw_flags = EXTENDED_STARTUPINFO_PRESENT;
        startup.startup_info.h_std_output = pipes.write_stdout;
        startup.startup_info.h_std_error = pipes.write_stderr;
        let mut cmdline = command_line(&command.program, &command.args);
        let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        create(&mut startup, cmdline.as_mut_ptr(), &mut pi)?;
        // 子进程已创建：关闭父侧写端副本。
        close_handle(pipes.write_stdout);
        close_handle(pipes.write_stderr);
        if pi.h_process.is_null() {
            return Err(SandboxError::Spawn(
                "CreateProcessW 未返回进程句柄".to_string(),
            ));
        }
        let assigned = unsafe { AssignProcessToJobObject(job, pi.h_process) };
        if assigned == FALSE {
            unsafe {
                TerminateProcess(pi.h_process, 1);
                WaitForSingleObject(pi.h_process, INFINITE);
            }
            return Err(SandboxError::Spawn(format!(
                "进程挂入 Job 失败（错误 {}），已终止",
                last_error()
            )));
        }
        Ok((pi, pipes))
    }

    /// 命令行拼接（lpCommandLine）：程序 + 参数；含空白参数加双引号。
    pub fn command_line(program: &str, args: &[String]) -> Vec<u16> {
        let joined = std::iter::once(program.to_string())
            .chain(args.iter().cloned())
            .map(|part| {
                if part.contains(' ') || part.contains('\t') {
                    format!("\"{}\"", part.replace('"', "\"\""))
                } else {
                    part
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        to_wide(&joined)
    }

    /// 管道对（父侧读端 + 子侧写端）。
    pub struct PipePair {
        pub read_stdout: Handle,
        pub read_stderr: Handle,
        pub write_stdout: Handle,
        pub write_stderr: Handle,
    }

    impl PipePair {
        pub fn create() -> std::io::Result<Self> {
            let mut read_stdout: Handle = std::ptr::null_mut();
            let mut write_stdout: Handle = std::ptr::null_mut();
            let mut read_stderr: Handle = std::ptr::null_mut();
            let mut write_stderr: Handle = std::ptr::null_mut();
            let attrs = SECURITY_ATTRIBUTES {
                n_length: std::mem::size_of::<SECURITY_ATTRIBUTES>() as DWORD,
                lp_security_descriptor: std::ptr::null_mut(),
                b_inherit_handle: TRUE,
            };
            let ok1 = unsafe {
                CreatePipe(
                    &mut read_stdout,
                    &mut write_stdout,
                    &attrs as *const _ as *const c_void,
                    0,
                )
            };
            let ok2 = unsafe {
                CreatePipe(
                    &mut read_stderr,
                    &mut write_stderr,
                    &attrs as *const _ as *const c_void,
                    0,
                )
            };
            if ok1 == FALSE || ok2 == FALSE {
                close_handle(read_stdout);
                close_handle(write_stdout);
                close_handle(read_stderr);
                close_handle(write_stderr);
                return Err(std::io::Error::last_os_error());
            }
            Ok(Self {
                read_stdout,
                read_stderr,
                write_stdout,
                write_stderr,
            })
        }
    }

    impl Drop for PipePair {
        fn drop(&mut self) {
            close_handle(self.read_stdout);
            close_handle(self.read_stderr);
            close_handle(self.write_stdout);
            close_handle(self.write_stderr);
        }
    }

    /// 管道句柄包装（raw handle 跨线程转移用；所有权唯一，可安全 Send）。
    #[derive(Clone, Copy)]
    pub struct PipeHandle(pub Handle);

    // 句柄值可跨线程转移（不并发使用即安全），标准 Windows 实践。
    unsafe impl Send for PipeHandle {}

    /// 读取管道（跨线程辅助：整体传递 PipeHandle，避免字段级捕获 raw 指针）。
    pub fn read_pipe_handle(handle: PipeHandle) -> Vec<u8> {
        read_pipe(handle.0)
    }

    /// 读管道直到 EOF。
    pub fn read_pipe(handle: Handle) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let mut read: DWORD = 0;
            let ok = unsafe {
                ReadFile(
                    handle,
                    buffer.as_mut_ptr(),
                    buffer.len() as DWORD,
                    &mut read,
                    std::ptr::null_mut(),
                )
            };
            if ok == FALSE || read == 0 {
                break;
            }
            out.extend_from_slice(&buffer[..read as usize]);
        }
        out
    }

    /// 进程是否存活（句柄可打开且退出码仍为 STILL_ACTIVE）。
    pub fn process_alive(pid: u32) -> bool {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION, FALSE, pid) };
        if handle.is_null() {
            return false;
        }
        let mut code: DWORD = 0;
        let ok = unsafe { GetExitCodeProcess(handle, &mut code) };
        close_handle(handle);
        ok == TRUE && code == STILL_ACTIVE
    }

    /// Job 内的进程（Job-only 用 std Child；OS 创建用 hProcess + 管道）。
    pub enum OsChild {
        StdChild {
            child: std::process::Child,
        },
        OsChild {
            pi: PROCESS_INFORMATION,
            stdout_read: Handle,
            stderr_read: Handle,
        },
    }

    impl OsChild {
        pub fn pid(&self) -> Option<u32> {
            match self {
                OsChild::StdChild { child } => Some(child.id()),
                OsChild::OsChild { pi, .. } => Some(pi.dw_process_id),
            }
        }

        pub fn wait(&mut self) -> Result<SandboxWaitInfo, SandboxError> {
            match self {
                OsChild::StdChild { child } => {
                    let mut stdout = Vec::new();
                    let mut stderr = Vec::new();
                    if let Some(mut out) = child.stdout.take() {
                        let _ = out.read_to_end(&mut stdout);
                    }
                    if let Some(mut err) = child.stderr.take() {
                        let _ = err.read_to_end(&mut stderr);
                    }
                    let status = child.wait().map_err(SandboxError::Io)?;
                    Ok(SandboxWaitInfo {
                        exit_code: status.code().unwrap_or(-1),
                        stdout,
                        stderr,
                    })
                }
                OsChild::OsChild {
                    pi,
                    stdout_read,
                    stderr_read,
                } => {
                    // 并行读两个管道（避免管道满死锁），再等进程退出。
                    let (stdout, stderr) = std::thread::scope(|scope| {
                        let out_handle = PipeHandle(*stdout_read);
                        let err_handle = PipeHandle(*stderr_read);
                        let t1 = scope.spawn(move || read_pipe_handle(out_handle));
                        let t2 = scope.spawn(move || read_pipe_handle(err_handle));
                        (t1.join().unwrap_or_default(), t2.join().unwrap_or_default())
                    });
                    unsafe {
                        WaitForSingleObject(pi.h_process, INFINITE);
                    }
                    let mut code: DWORD = 0;
                    unsafe {
                        GetExitCodeProcess(pi.h_process, &mut code);
                    }
                    Ok(SandboxWaitInfo {
                        exit_code: code as i32,
                        stdout,
                        stderr,
                    })
                }
            }
        }

        pub fn kill(&mut self) {
            match self {
                OsChild::StdChild { child } => {
                    let _ = child.kill();
                }
                OsChild::OsChild { pi, .. } => unsafe {
                    TerminateProcess(pi.h_process, 1);
                },
            }
        }
    }

    impl Drop for OsChild {
        fn drop(&mut self) {
            match self {
                OsChild::StdChild { child } => {
                    // 丢弃时若仍运行则终止（防孤儿）。
                    let _ = child.kill();
                }
                OsChild::OsChild {
                    pi,
                    stdout_read,
                    stderr_read,
                } => unsafe {
                    TerminateProcess(pi.h_process, 1);
                    WaitForSingleObject(pi.h_process, INFINITE);
                    close_handle(pi.h_process);
                    close_handle(pi.h_thread);
                    close_handle(*stdout_read);
                    close_handle(*stderr_read);
                },
            }
        }
    }

    /// Windows 进程内部句柄（inner：进程 + Job）。
    pub struct WindowsProcess {
        pub os_child: OsChild,
        pub job: Handle,
    }

    // 句柄值可跨线程转移（进程/Job 句柄由 WindowsProcess 独占管理），标准 Windows 实践。
    unsafe impl Send for WindowsProcess {}
    unsafe impl Send for OsChild {}

    impl SandboxProcessInner for WindowsProcess {
        fn wait(&mut self) -> Result<SandboxWaitInfo, SandboxError> {
            self.os_child.wait()
        }

        fn kill(&mut self) -> Result<(), SandboxError> {
            self.os_child.kill();
            terminate_job(self.job, 1);
            Ok(())
        }
    }

    impl Drop for WindowsProcess {
        fn drop(&mut self) {
            terminate_job(self.job, 1);
            close_handle(self.job);
        }
    }

    impl SandboxExecutor for WindowsSandboxExecutor {
        fn name(&self) -> &'static str {
            "windows-job"
        }

        fn capability(&self) -> IsolationLevel {
            super::available_isolation(&self.support)
        }

        fn spawn(&self, command: &SandboxCommand) -> Result<SandboxProcess, SandboxError> {
            // 网络 egress 边界（R9）：AllowList/Unrestricted 网络策略只能在
            // AppContainer 路径强制；仅 Job/LowIL 隔离无法限制网络 → 显式拒绝。
            let uses_app_container = command.policy.require_isolation
                >= IsolationLevel::AppContainerJob
                && self.support.app_container;
            if network_requires_app_container(&command.policy) && !uses_app_container {
                return Err(SandboxError::Unsupported(format!(
                    "网络策略 {:?} 需要 AppContainer 隔离才能强制网络白名单，\
                     当前执行路径仅提供 {:?}（显式拒绝，不静默放开网络）",
                    command.policy.network_policy,
                    super::available_isolation(&self.support)
                )));
            }
            let job = create_job(&command.policy).ok_or_else(|| {
                SandboxError::Unsupported(format!("Job Object 创建失败（错误 {}）", last_error()))
            })?;
            let os_child = self.create_process(command, job)?;
            let pid = os_child.pid().unwrap_or(0);
            Ok(SandboxProcess {
                handle: SandboxHandle {
                    id: format!("win-{pid}"),
                    spawned_at: Utc::now().to_rfc3339(),
                },
                status: SandboxProcessStatus::Running,
                stdout: Vec::new(),
                stderr: Vec::new(),
                inner: Some(Box::new(WindowsProcess { os_child, job })),
            })
        }

        fn kill(&self, _handle: &SandboxHandle) -> Result<(), SandboxError> {
            Ok(())
        }

        fn check_healthy(&self) -> SandboxHealth {
            SandboxHealth {
                healthy: true,
                detail: "Windows Job 执行器可用".to_string(),
            }
        }

        fn attach(&self, policy: &SandboxPolicy, pid: u32) -> Result<JobGuard, SandboxError> {
            let job = create_job(policy).ok_or_else(|| {
                SandboxError::Unsupported(format!("Job Object 创建失败（错误 {}）", last_error()))
            })?;
            if !assign_pid_to_job(job, pid) {
                close_handle(job);
                return Err(SandboxError::Spawn(format!(
                    "进程 {pid} 无法挂入 Job（错误 {}）",
                    last_error()
                )));
            }
            Ok(JobGuard { pid, job })
        }
    }

    /// 结构布局断言（与 Windows SDK 一致；防 ABI 漂移）。
    pub(crate) fn assert_struct_layouts() -> bool {
        let mut ok = true;
        if std::mem::size_of::<usize>() == 8 {
            // x64 期望值（与 SDK 编译对齐一致）。
            ok &= std::mem::size_of::<JOBOBJECT_BASIC_LIMIT_INFORMATION>() == 64;
            ok &= std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() == 144;
            ok &= std::mem::size_of::<SYSTEM_MANDATORY_LABEL_ACE>() == 20;
            ok &= std::mem::size_of::<STARTUPINFOW>() == 104;
            ok &= std::mem::size_of::<STARTUPINFOEXW>() == 112;
            ok &= std::mem::size_of::<CREDENTIALW>() == 80;
        } else {
            // x86 期望值。
            ok &= std::mem::size_of::<JOBOBJECT_BASIC_LIMIT_INFORMATION>() == 48;
            ok &= std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() == 112;
        }
        ok
    }
}
