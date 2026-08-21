use async_trait::async_trait;
use clap::{Args, Parser, Subcommand};
use colored::Colorize;
use owo_agent_core::permissions::{Approver, AutoApprover, Decision, PermissionRequest, Policy};
use owo_agent_core::session::{Session, SessionStore};
use owo_agent_core::tools::ToolRegistry;
use owo_agent_core::{
    builtin_suite, discover_plugins, eval_suite_path, export_html, export_markdown,
    install_builtin_packages, list_traces, load_trace, run_suite, save_trace, Agent, AgentConfig,
    ChatMessage, LearnRecorder, McpClient, McpServerConfig, ModelOutput, ModelProvider,
    OpenAiCompatibleConfig, OpenAiCompatibleProvider, PluginManifest, ProactiveEngine, Settings,
    SituationStore, SkillRegistry, SqliteSessionStore, SuggestionAction, ToolSpec, TraceRecord,
    TurnEvent, Whitelist,
};
use rustyline::error::ReadlineError;
use std::collections::HashSet;
use std::future::Future;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

mod tui;

/// 千问 Token Plan 的默认通用文本/推理模型；可在工作台设置中覆盖。
const DEFAULT_MODEL: &str = "qwen3.8-max";

const AGENTS_TEMPLATE: &str = r#"# AGENTS.md

<!-- 由 owo-agent /init 生成，按项目实际情况修改。
     该文件会被 Agent 在每次会话开始时注入，作为项目级规则。 -->

## 项目说明

- 一句话描述本项目做什么。

## 开发规则

- 写清楚构建命令、测试命令与代码约定。
- 说明哪些目录/文件禁止修改。
"#;

fn apply_egress_setting(settings: &Settings) {
    if !settings.egress.cloud_enabled {
        std::env::set_var("OWO_CLOUD_ENABLED", "false");
    }
}

/// 把 settings.json 的禁用技能列表注入技能注册表（进程内共享集合，Web 切换即时生效）。
fn apply_disabled_skills(skills: &mut SkillRegistry, settings: &Settings) {
    let disabled = Arc::new(Mutex::new(
        settings
            .skills
            .disabled
            .iter()
            .cloned()
            .collect::<HashSet<_>>(),
    ));
    skills.set_disabled(disabled);
}

/// 开发环境下的内置技能包根目录：`<repo>/agent-sdk/skills`。
fn builtin_skills_root() -> PathBuf {
    if let Ok(dir) = std::env::var("OWO_SKILLS_DIR") {
        return PathBuf::from(dir);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("skills");
            if candidate.is_dir() {
                return candidate;
            }
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|parent| parent.parent())
        .map(|root| root.join("skills"))
        .unwrap_or_else(|| PathBuf::from("skills"))
}

#[derive(Parser)]
#[command(
    name = "owo-agent",
    version,
    about = "OwO Agent SDK CLI（Codex 式 / OpenCode 式交互终端）"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// 执行一轮一次性任务（非交互）
    Turn(TurnArgs),
    /// 启动本地 HTTP API 服务
    Serve(ServeArgs),
    /// 进入交互式终端（默认命令）
    Repl(ReplArgs),
    /// 进入全屏 TUI（OpenCode 风格）
    Tui(tui::TuiArgs),
    /// 生成 AGENTS.md 项目规则文件
    Init(InitArgs),
    /// 运行评估套件（内置 demo 或自定义 JSON）
    Eval(EvalArgs),
    /// 本机 IPC 往返延迟基准
    Bench(BenchArgs),
    /// 云端执行任务（M4a：提交/列表/状态/diff/应用/回滚）
    Cloud(CloudArgs),
    /// 插件市场治理（M4b：catalog/check/install/update/uninstall/verify；本地离线模式）
    Plugin(PluginArgs),
    /// 审计链校验/导出（R6 audit_chain：verify 检出篡改，export 输出可离线校验的导出文件）
    Audit(AuditArgs),
    /// 数据备份（R9：zip 打包 index.db/settings/notes/skills/workflows，复用服务端备份逻辑）
    Backup(BackupArgs),
    /// 环境诊断（R10：数据目录/凭据/模型/端点/服务健康逐项检查）
    Doctor(DoctorArgs),
}

/// `owo-agent backup`：本地一键备份（同 HTTP POST /storage/backup 的打包逻辑）。
#[derive(Args)]
struct BackupArgs {
    /// 输出 zip 路径（缺省 <data>/backups/backup-<时间戳>.zip）
    #[arg(long)]
    out: Option<std::path::PathBuf>,
}

/// `owo-agent doctor`：环境健康诊断（数据目录/凭据/模型/端点/服务）。
#[derive(Args)]
struct DoctorArgs {
    /// 数据目录（缺省 OWO_AGENT_DATA 或 <workspace>/.owo-data）
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// 目标工作区（缺省当前目录）
    #[arg(long)]
    workspace: Option<PathBuf>,
}

/// `owo-agent audit`：verify|export（复用 core audit_chain::run_audit_cli）。
#[derive(Args)]
struct AuditArgs {
    #[command(subcommand)]
    action: AuditAction,
    /// 审计链 HMAC 密钥（hex 字符串；缺省读 OWO_AUDIT_KEY 环境变量）
    #[arg(long)]
    key: Option<String>,
    /// 审计链密钥文件路径（hex 文本；与 --key 二选一）
    #[arg(long)]
    key_file: Option<String>,
}

#[derive(Subcommand)]
enum AuditAction {
    /// 校验导出文件的链完整性（检出任意篡改）
    Verify { path: String },
    /// 把已导出审计文件另存为 out（可离线分发校验）
    Export { path: String, out: String },
}

/// 解析审计链密钥：--key → OWO_AUDIT_KEY → --key-file。缺省明确报错（不 panic）。
fn audit_key(args: &AuditArgs) -> Result<Vec<u8>, String> {
    let hex = if let Some(key) = &args.key {
        Some(key.clone())
    } else if let Ok(env) = std::env::var("OWO_AUDIT_KEY") {
        if env.trim().is_empty() {
            None
        } else {
            Some(env)
        }
    } else {
        None
    };
    let hex = match hex {
        Some(hex) => hex,
        None => {
            if let Some(path) = &args.key_file {
                let content =
                    std::fs::read_to_string(path).map_err(|e| format!("读取密钥文件失败：{e}"))?;
                content.trim().to_string()
            } else {
                return Err(
                    "缺少审计链密钥：请用 --key <hex> 或设置 OWO_AUDIT_KEY 环境变量".to_string(),
                );
            }
        }
    };
    let hex = hex.trim();
    if hex.len() % 2 != 0 {
        return Err("密钥必须为偶数长度 hex 字符串".to_string());
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| format!("密钥不是合法 hex：{e}"))
        })
        .collect()
}

/// `owo-agent audit verify|export` 命令入口。
fn run_audit_cmd(args: AuditArgs) -> Result<(), Box<dyn std::error::Error>> {
    use owo_agent_core::audit_chain::{run_audit_cli, AuditCliCommand};
    let key = audit_key(&args)?;
    let command = match args.action {
        AuditAction::Verify { path } => AuditCliCommand::Verify { path },
        AuditAction::Export { path, out } => AuditCliCommand::Export { path, out },
    };
    let outcome = run_audit_cli(&command, &key)?;
    match outcome {
        owo_agent_core::audit_chain::AuditCliOutcome::VerifyOk { records, anchors } => {
            println!("审计链校验通过：{records} 条记录 / {anchors} 个锚点");
        }
        owo_agent_core::audit_chain::AuditCliOutcome::Exported { out, records } => {
            println!("审计导出完成：{out}（{records} 条记录，含链锚点）");
        }
    }
    Ok(())
}

#[derive(Args)]
struct PluginArgs {
    #[command(subcommand)]
    action: PluginAction,
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
    /// 数据目录（默认 OWO_AGENT_DATA 或 %LOCALAPPDATA%\OwO\Agent）
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// 远端市场 URL（预留；当前实现为本地离线模式）
    #[arg(long)]
    url: Option<String>,
    /// 离线模式（默认 true；不联网）
    #[arg(long)]
    offline: bool,
}

#[derive(Subcommand)]
enum PluginAction {
    /// 列出本地插件目录
    Catalog,
    /// 校验插件目录（签名/扫描/版本）
    Check { dir: PathBuf },
    /// 校验插件目录（与 check 相同）
    Verify { dir: PathBuf },
    /// 安装插件目录
    Install { dir: PathBuf },
    /// 更新已安装插件（id 为已安装插件 id）
    Update { id: String, dir: PathBuf },
    /// 卸载插件
    Uninstall { id: String },
}

#[derive(Args)]
struct TurnArgs {
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
    #[arg(long)]
    prompt: String,
    #[arg(long)]
    model: Option<String>,
    /// 自动允许所有审批（仅测试用）
    #[arg(long)]
    no_approval: bool,
}

#[derive(Args)]
struct ServeArgs {
    #[arg(long, default_value_t = 4096)]
    port: u16,
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
}

#[derive(Args)]
struct ReplArgs {
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
    #[arg(long)]
    model: Option<String>,
    /// 初始 agent：build（默认）或 plan（只读）
    #[arg(long, default_value = "build")]
    agent: String,
    /// 自动允许所有审批（仅测试用）
    #[arg(long)]
    no_approval: bool,
    /// 覆盖数据目录（默认 %LOCALAPPDATA%\OwO\Agent 或 OWO_AGENT_DATA）
    #[arg(long)]
    data_dir: Option<PathBuf>,
}

#[derive(Args)]
struct InitArgs {
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
    /// 已存在时覆盖
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
struct EvalArgs {
    /// 自定义套件 JSON 路径（缺省使用内置 demo 套件）
    #[arg(long)]
    suite: Option<PathBuf>,
    #[arg(long)]
    model: Option<String>,
}

#[derive(Args)]
struct BenchArgs {
    #[arg(long, default_value_t = 200)]
    requests: usize,
}

#[derive(Args)]
struct CloudArgs {
    /// 任务队列持久化目录（默认 %TEMP%\owo-cloud-queue）
    #[arg(long)]
    dir: Option<PathBuf>,
    /// 传输后端：mock（本地模拟，不联网，默认）或 http（远端，需 --url）
    #[arg(long, default_value = "mock")]
    transport: String,
    /// HTTP 远端 base URL（--transport http 时必填，凭据经 OWO_CLOUD_TOKEN 环境变量）
    #[arg(long)]
    url: Option<String>,
    #[command(subcommand)]
    command: CloudCommand,
}

#[derive(Subcommand)]
enum CloudCommand {
    /// 提交云端任务（--command 可重复，顺序执行）
    Submit(CloudSubmitArgs),
    /// 列出全部任务与状态
    List,
    /// 查看任务状态
    Status(CloudIdArgs),
    /// 查看任务 diff（回传的变更清单）
    Diff(CloudIdArgs),
    /// 把任务 diff 应用到本地工作区
    Apply(CloudApplyArgs),
    /// 回滚已应用的 diff
    Revert(CloudApplyArgs),
}

#[derive(Args)]
struct CloudSubmitArgs {
    /// 工作区快照目录
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
    /// 执行命令（可重复，顺序执行）
    #[arg(long, required = true)]
    command: Vec<String>,
    /// 单条命令超时（秒）
    #[arg(long, default_value_t = 60)]
    timeout: u64,
    /// 任务名
    #[arg(long)]
    name: Option<String>,
    /// 提交后立即执行（否则仅入队）
    #[arg(long)]
    run: bool,
}

#[derive(Args)]
struct CloudIdArgs {
    task_id: String,
}

#[derive(Args)]
struct CloudApplyArgs {
    task_id: String,
    /// 应用/回滚目标目录
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        None => run_async(Repl::run(ReplArgs {
            workspace: PathBuf::from("."),
            model: None,
            agent: "build".to_string(),
            no_approval: false,
            data_dir: None,
        }))?,
        Some(Commands::Turn(args)) => run_async(run_turn(args))?,
        Some(Commands::Serve(args)) => run_async(run_serve(args))?,
        Some(Commands::Repl(args)) => run_async(Repl::run(args))?,
        Some(Commands::Tui(args)) => tui::run(args)?,
        Some(Commands::Init(args)) => run_init(args)?,
        Some(Commands::Eval(args)) => run_async(run_eval(args))?,
        Some(Commands::Bench(args)) => run_async(run_bench(args))?,
        Some(Commands::Cloud(args)) => run_async(run_cloud(args))?,
        Some(Commands::Plugin(args)) => run_plugin(args)?,
        Some(Commands::Audit(args)) => run_audit_cmd(args)?,
        Some(Commands::Backup(args)) => run_backup_cmd(args)?,
        Some(Commands::Doctor(args)) => run_async(run_doctor_cmd(args))?,
    }
    Ok(())
}

/// `owo-agent doctor`：逐项环境诊断，输出 [ok]/[warn]/[fail] 清单；任一 fail 非零退出。
async fn run_doctor_cmd(args: DoctorArgs) -> Result<(), Box<dyn std::error::Error>> {
    let workspace = args
        .workspace
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let data_root = if let Some(dir) = args.data_dir {
        dir
    } else if let Ok(env_dir) = std::env::var("OWO_AGENT_DATA") {
        PathBuf::from(env_dir)
    } else {
        workspace.join(".owo-data")
    };
    let mut failures = 0usize;
    let mut checks: Vec<(&str, bool, String)> = Vec::new();

    // 1) 数据目录与关键存储文件。
    let storage_ok = data_root.is_dir() || std::fs::create_dir_all(&data_root).is_ok();
    checks.push(("数据目录", storage_ok, data_root.display().to_string()));
    let index_db = data_root.join("index.db");
    if index_db.exists() {
        let openable = owo_agent_core::sqlite_store::SqliteSessionStore::open(&index_db).is_ok();
        checks.push(("SQLite index.db", openable, index_db.display().to_string()));
    } else {
        checks.push((
            "SQLite index.db",
            true,
            "未初始化（首次运行自动创建）".to_string(),
        ));
    }

    // 2) 模型凭据与端点（缺省不视为失败，仅提示）。
    match std::env::var("OPENAI_API_KEY") {
        Ok(value) if !value.trim().is_empty() => {
            checks.push(("OPENAI_API_KEY", true, "已配置".to_string()));
        }
        _ => {
            let base_url = std::env::var("OPENAI_BASE_URL").unwrap_or_default();
            let local = base_url.contains("127.0.0.1") || base_url.contains("localhost");
            checks.push((
                "OPENAI_API_KEY",
                local,
                if local {
                    "本地端点免凭据".to_string()
                } else {
                    "未配置（云端模型不可用）".to_string()
                },
            ));
        }
    }

    // 3) 模型网关韧性配置（R9：熔断/重试参数）。
    let mut gateway_note = String::new();
    for (name, default) in [
        ("OWO_MODEL_RETRY_MAX", "3"),
        ("OWO_MODEL_CIRCUIT_THRESHOLD", "5"),
    ] {
        match std::env::var(name) {
            Ok(value) => gateway_note.push_str(&format!("{name}={value} ")),
            Err(_) => gateway_note.push_str(&format!("{name}={default}（默认） ")),
        }
    }
    checks.push(("模型网关韧性", true, gateway_note.trim().to_string()));

    // 4) 服务健康（serve 冒烟：可选项）。
    match std::env::var("OWO_DOCTOR_SERVE_PORT") {
        Ok(port) => {
            let url = format!("http://127.0.0.1:{port}/health");
            let token = std::env::var("OWO_DOCTOR_SERVE_TOKEN").unwrap_or_default();
            let response = reqwest::Client::new()
                .get(&url)
                .header("authorization", format!("Bearer {token}"))
                .timeout(std::time::Duration::from_secs(3))
                .send()
                .await;
            match response {
                Ok(resp) => {
                    checks.push(("本地服务 /health", resp.status().is_success(), url));
                }
                Err(error) => {
                    checks.push(("本地服务 /health", false, format!("{url}（{error}）")));
                }
            }
        }
        Err(_) => {
            checks.push((
                "本地服务 /health",
                true,
                "跳过（设置 OWO_DOCTOR_SERVE_PORT 启用）".to_string(),
            ));
        }
    }

    // 5) 备份目录可写（release 产物路径）。
    let backups = data_root.join("backups");
    let backups_ok = backups.is_dir() || std::fs::create_dir_all(&backups).is_ok();
    checks.push(("备份目录", backups_ok, backups.display().to_string()));

    for (name, ok, note) in &checks {
        let mark = if *ok { "[ok]  " } else { "[fail]" };
        println!("{mark} {name}：{note}");
        if !*ok {
            failures += 1;
        }
    }
    if failures > 0 {
        println!("诊断完成：{} 项失败", failures);
        std::process::exit(1);
    }
    println!("诊断完成：全部通过");
    Ok(())
}

/// `owo-agent backup`：复用服务端 backup.rs 打包逻辑，本地一键备份。
fn run_backup_cmd(args: BackupArgs) -> Result<(), Box<dyn std::error::Error>> {
    let workspace = std::env::current_dir()?;
    let root = ensure_data_root(None, &workspace);
    let zip_bytes = owo_agent_server::backup::build_backup_zip(&root, &workspace)?;
    let out = match args.out {
        Some(path) => path,
        None => {
            let dir = root.join("backups");
            std::fs::create_dir_all(&dir)?;
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or_default();
            dir.join(format!("backup-{stamp}.zip"))
        }
    };
    std::fs::write(&out, &zip_bytes)?;
    println!("备份完成：{}（{} 字节）", out.display(), zip_bytes.len());
    Ok(())
}

/// 插件市场治理（本地离线模式）：catalog/check/install/update/uninstall/verify。
/// 签名/扫描/回滚由 core PluginManager 提供；远端拉取走 HTTP 面（POST /plugins/market/refresh）。
fn run_plugin(args: PluginArgs) -> Result<(), Box<dyn std::error::Error>> {
    use owo_agent_core::plugin::{
        discover_plugins, scan_plugin_for_risks, PluginManager, PluginManifest,
    };

    let workspace = args.workspace;
    let data_root = if let Some(dir) = args.data_dir {
        dir
    } else if let Ok(env_dir) = std::env::var("OWO_AGENT_DATA") {
        std::path::PathBuf::from(env_dir)
    } else if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        std::path::PathBuf::from(local).join("OwO").join("Agent")
    } else {
        std::path::PathBuf::from("data")
    };
    let app_version = env!("CARGO_PKG_VERSION").to_string();
    let _ = &args.url; // 远端 URL 预留：当前为本地离线模式（--offline 恒真）。
    let _ = args.offline;

    match args.action {
        PluginAction::Catalog => {
            let plugins = discover_plugins(&workspace, &data_root);
            println!(
                "本地插件目录（workspace={} data={}）：",
                workspace.display(),
                data_root.display()
            );
            for (path, manifest) in &plugins {
                let base = path.parent().unwrap_or(path);
                let manifest_content = std::fs::read_to_string(path).unwrap_or_default();
                let entry_content = manifest
                    .entry
                    .as_ref()
                    .and_then(|entry| std::fs::read_to_string(base.join(entry)).ok());
                let risks = scan_plugin_for_risks(
                    &manifest_content,
                    entry_content.as_deref(),
                    &manifest.network_allowlist,
                );
                let risk = if risks.is_empty() { "clean" } else { "RISK" };
                println!(
                    "  {} v{}（{}）[{}]{}",
                    manifest.id,
                    manifest.version,
                    manifest.name,
                    risk,
                    if risks.is_empty() {
                        String::new()
                    } else {
                        format!("：{}", risks.join("；"))
                    }
                );
            }
            if plugins.is_empty() {
                println!("  （无插件）");
            }
        }
        PluginAction::Check { dir } | PluginAction::Verify { dir } => {
            let manager = PluginManager::new(data_root.clone(), app_version);
            match manager.verify_plugin_dir(&dir) {
                Ok(report) => {
                    println!(
                        "校验通过：{} v{}（{:?}）",
                        report.id, report.version, report.state
                    );
                    for line in report.audit {
                        println!("  {line}");
                    }
                }
                Err(error) => {
                    println!("校验失败：{error}");
                    std::process::exit(1);
                }
            }
        }
        PluginAction::Install { dir } => {
            let manager = PluginManager::new(data_root.clone(), app_version);
            match manager.install(&dir) {
                Ok(report) => {
                    println!(
                        "安装完成：{} v{}（{:?}）",
                        report.id, report.version, report.state
                    );
                    for line in report.audit {
                        println!("  {line}");
                    }
                }
                Err(error) => {
                    println!("安装失败：{error}");
                    std::process::exit(1);
                }
            }
        }
        PluginAction::Update { id, dir } => {
            let manager = PluginManager::new(data_root.clone(), app_version);
            let backup = data_root.join("plugins").join("backups");
            match manager.update(&dir, &backup) {
                Ok(report) => {
                    println!("更新完成：{id} → v{}（{:?}）", report.version, report.state);
                    for line in report.audit {
                        println!("  {line}");
                    }
                }
                Err(error) => {
                    println!("更新失败（已回滚或旧版保留）：{error}");
                    std::process::exit(1);
                }
            }
        }
        PluginAction::Uninstall { id } => {
            let manager = PluginManager::new(data_root.clone(), app_version);
            match manager.uninstall(&id) {
                Ok(audit_lines) => {
                    println!("已卸载 {id}");
                    for line in audit_lines {
                        println!("  {line}");
                    }
                }
                Err(error) => {
                    println!("卸载失败：{error}");
                    std::process::exit(1);
                }
            }
        }
    }
    let _ = PluginManifest::load; // 类型引用保活（防未使用告警变体依赖）。
    Ok(())
}

/// 云端执行任务（M4a）：队列持久化 + Mock/HTTP 传输 + diff 应用/回滚。
async fn run_cloud(args: CloudArgs) -> Result<(), Box<dyn std::error::Error>> {
    use owo_agent_core::cloud_exec::{
        CloudTaskQueue, CloudTaskSpec, HttpTransport, MockRemoteTransport, NullSink,
    };

    let queue_dir = args.dir.unwrap_or_else(|| {
        std::env::temp_dir()
            .join("owo-cloud-queue")
            .join(format!("{}", std::process::id()))
    });
    let transport: Box<dyn owo_agent_core::cloud_exec::CloudTransport> =
        match args.transport.as_str() {
            "mock" => Box::new(MockRemoteTransport::new(
                std::env::temp_dir()
                    .join("owo-cloud-remote")
                    .join(format!("{}", std::process::id())),
            )),
            "http" => {
                let url = args
                    .url
                    .clone()
                    .ok_or("--transport http 需要 --url <base>")?;
                Box::new(HttpTransport::new(url)?)
            }
            other => return Err(format!("未知传输后端：{other}（可选 mock|http）").into()),
        };
    let mut queue = CloudTaskQueue::new(queue_dir.clone(), transport);
    queue.recover()?;

    match args.command {
        CloudCommand::Submit(submit) => {
            let spec = CloudTaskSpec {
                name: submit.name.unwrap_or_default(),
                workspace_dir: submit.workspace.clone(),
                commands: submit.command.clone(),
                env_passthrough: vec![],
                timeout_secs: submit.timeout,
            };
            let task_id = queue.submit(spec)?;
            println!(
                "已提交任务：{task_id}（传输={} 队列={}）",
                queue.transport_kind(),
                queue_dir.display()
            );
            if submit.run {
                while let Some(id) = queue.run_next(&NullSink).await? {
                    let record = queue.record(&id).unwrap();
                    let diff_count = record.result.as_ref().map(|r| r.diff.len()).unwrap_or(0);
                    let error = record
                        .last_error
                        .as_deref()
                        .map(|e| format!("（{e}）"))
                        .unwrap_or_default();
                    println!(
                        "任务 {id} → {:?}（diff 条目={diff_count}）{error}",
                        record.state
                    );
                    if id == task_id {
                        break;
                    }
                }
            }
        }
        CloudCommand::List => {
            for record in queue.list() {
                let diff_count = record.result.as_ref().map(|r| r.diff.len()).unwrap_or(0);
                println!(
                    "{:12} {:9} retries={} diff={} {}",
                    record.task_id,
                    format!("{:?}", record.state),
                    record.retry_count,
                    diff_count,
                    record.spec.name
                );
            }
        }
        CloudCommand::Status(id) => {
            let record = queue
                .record(&id.task_id)
                .ok_or(format!("任务不存在：{}", id.task_id))?;
            let result = record.result.as_ref();
            println!("task_id:   {}", record.task_id);
            println!("state:     {:?}", record.state);
            println!("retries:   {}", record.retry_count);
            println!(
                "exit_code: {}",
                result.map(|r| r.exit_code.unwrap_or(-1)).unwrap_or(-1)
            );
            if let Some(error) = &record.last_error {
                println!("error:     {error}");
            }
        }
        CloudCommand::Diff(id) => {
            let diffs = queue
                .diff(&id.task_id)
                .ok_or(format!("任务不存在或无结果：{}", id.task_id))?;
            for diff in diffs {
                println!("{:8} {}", format!("{:?}", diff.change), diff.path);
            }
            if diffs.is_empty() {
                println!("（无变更）");
            }
        }
        CloudCommand::Apply(id) => {
            let applied = queue
                .apply_to(&id.task_id, &id.workspace)
                .await
                .map_err(|(n, e)| format!("diff 应用失败（已应用 {n} 条）：{e}"))?;
            println!("已应用 {applied} 条 diff 到 {}", id.workspace.display());
        }
        CloudCommand::Revert(id) => {
            let reverted = queue.revert_from(&id.task_id, &id.workspace).await?;
            println!("已回滚 {reverted} 条 diff（{}）", id.workspace.display());
        }
    }
    Ok(())
}

async fn run_eval(args: EvalArgs) -> Result<(), Box<dyn std::error::Error>> {
    let model = resolve_model(args.model, None);
    let mut config = OpenAiCompatibleConfig::from_env()?;
    config.model = model.clone();
    // R9：模型网关韧性（重试/熔断/failover）。
    let provider = std::sync::Arc::new(owo_agent_core::gateway::ResilientProvider::from_config(
        config,
    )?);
    let suite = match args.suite {
        Some(path) => {
            eval_suite_path(&path).ok_or_else(|| format!("评估套件解析失败：{}", path.display()))?
        }
        None => builtin_suite(),
    };
    println!(
        "运行评估套件：{}（{} 个用例）",
        suite.name,
        suite.cases.len()
    );
    let report = run_suite(provider, &model, &suite).await;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

struct StubProvider;

#[async_trait]
impl ModelProvider for StubProvider {
    async fn complete(
        &self,
        _messages: &[ChatMessage],
        _tools: &[ToolSpec],
    ) -> Result<ModelOutput, String> {
        Err("bench stub".to_string())
    }
}

async fn run_bench(args: BenchArgs) -> Result<(), Box<dyn std::error::Error>> {
    let provider = std::sync::Arc::new(StubProvider);
    let registry = ToolRegistry::new();
    let policy = Policy::new(std::env::temp_dir());
    let agent = Agent::new(provider, registry, policy, AgentConfig::default());
    let root = std::env::temp_dir().join(format!("owo-bench-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root)?;
    let store = SqliteSessionStore::open(&root.join("index.db"))?;
    let state = Arc::new(owo_agent_server::AppState::new(
        agent,
        store,
        root.join("traces"),
        root.clone(),
        std::env::temp_dir(),
    ));
    let app = owo_agent_server::build_router(state);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let addr = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::new();
    let mut durations = Vec::new();
    for _ in 0..args.requests {
        let start = std::time::Instant::now();
        let response = client.get(format!("http://{addr}/health")).send().await?;
        assert_eq!(response.status().as_u16(), 200);
        durations.push(start.elapsed().as_micros() as u64);
    }
    durations.sort_unstable();
    let p50 = durations[durations.len() / 2];
    let index95 = ((durations.len() as f64 * 0.95) as usize).saturating_sub(1);
    let p95 = durations[index95].max(p50);
    println!(
        "{}",
        serde_json::json!({
            "requests": durations.len(),
            "p50_us": p50,
            "p95_us": p95,
            "max_us": durations.last().copied().unwrap_or(0),
        })
    );
    server.abort();
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

fn run_async<F>(future: F) -> Result<(), Box<dyn std::error::Error>>
where
    F: Future<Output = Result<(), Box<dyn std::error::Error>>>,
{
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(future)
}

fn resolve_model(option: Option<String>, settings_model: Option<&str>) -> String {
    option
        .or_else(|| std::env::var("OPENAI_MODEL").ok())
        .or_else(|| settings_model.map(str::to_string))
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

fn build_agent(
    workspace: &std::path::Path,
    model: &str,
    read_only: bool,
) -> Result<Agent, Box<dyn std::error::Error>> {
    let root = ensure_data_root(None, workspace);
    let settings = Settings::load(workspace);
    let mut skills = SkillRegistry::discover(workspace, &root);
    apply_disabled_skills(&mut skills, &settings);
    build_agent_with_mcp(
        workspace,
        model,
        read_only,
        &[],
        &skills,
        &settings.deny_commands,
    )
}

fn build_agent_with_mcp(
    workspace: &std::path::Path,
    model: &str,
    read_only: bool,
    mcp_clients: &[(String, Arc<tokio::sync::Mutex<McpClient>>)],
    skills: &SkillRegistry,
    deny_commands: &[String],
) -> Result<Agent, Box<dyn std::error::Error>> {
    let mut config = OpenAiCompatibleConfig::from_env()?;
    config.model = model.to_string();
    // R9：模型网关韧性（重试/退避/熔断/failover 强→次选云→本地）。
    let provider = Arc::new(owo_agent_core::gateway::ResilientProvider::from_config(
        config,
    )?);
    let mut policy = if read_only {
        Policy::read_only(workspace.to_path_buf())
    } else {
        Policy::new(workspace.to_path_buf())
    };
    for fragment in deny_commands {
        policy.add_deny_command(fragment.clone());
    }
    let mut config = AgentConfig::default();
    if let Ok(value) = std::env::var("OWO_TOKEN_BUDGET") {
        if let Ok(budget) = value.parse() {
            config.token_budget = budget;
        }
    }
    if let Ok(value) = std::env::var("OWO_KEEP_RECENT") {
        if let Ok(keep) = value.parse() {
            config.keep_recent = keep;
        }
    }
    let mut agent = Agent::new(provider, ToolRegistry::new(), policy, config);
    // 统一走 Agent::register_mcp_tools：记录客户端到进程生命周期注册表（进程级热卸载），
    // 同时把工具挂进 Agent 注册表。
    for (server_name, client) in mcp_clients {
        let tools = client
            .try_lock()
            .map_err(|_| format!("MCP 客户端 {server_name} 忙碌"))?
            .tools();
        agent.register_mcp_tools(server_name, Arc::clone(client), tools);
    }
    agent.set_skills(skills.clone());
    attach_auto_review(&mut agent, model);
    Ok(agent)
}

/// 独立审批模型（Auto-review）：
/// - 默认只挂启发式预筛（零模型成本，命中已知注入/高危模式直接 Deny）；
/// - `OWO_AUTO_REVIEW=1` 时追加独立模型复审（`OWO_REVIEW_MODEL` 可选覆盖）。
fn attach_auto_review(agent: &mut Agent, model: &str) {
    if std::env::var("OWO_AUTO_REVIEW").as_deref() == Ok("1") {
        let mut config = match OpenAiCompatibleConfig::from_env() {
            Ok(config) => config,
            Err(error) => {
                eprintln!("警告：Auto-review 模型初始化失败（{error}），仅启用启发式预筛");
                agent.set_reviewer(Some(Arc::new(owo_agent_core::AutoReviewChain::new(None))));
                return;
            }
        };
        config.model = std::env::var("OWO_REVIEW_MODEL").unwrap_or_else(|_| model.to_string());
        let provider = match OpenAiCompatibleProvider::new(config) {
            Ok(provider) => Arc::new(provider),
            Err(error) => {
                eprintln!("警告：Auto-review 模型初始化失败（{error}），仅启用启发式预筛");
                agent.set_reviewer(Some(Arc::new(owo_agent_core::AutoReviewChain::new(None))));
                return;
            }
        };
        agent.set_reviewer(Some(Arc::new(owo_agent_core::AutoReviewChain::from_model(
            provider,
        ))));
    } else {
        agent.set_reviewer(Some(Arc::new(owo_agent_core::AutoReviewChain::new(None))));
    }
}

async fn connect_mcp_clients(
    configs: &[McpServerConfig],
) -> Vec<(String, Arc<tokio::sync::Mutex<McpClient>>)> {
    let mut clients = Vec::new();
    for config in configs {
        match McpClient::connect(config).await {
            Ok(client) => {
                println!(
                    "{} MCP {}（工具 {} 个）",
                    "已连接".green(),
                    config.name,
                    client.tools().len()
                );
                clients.push((
                    config.name.clone(),
                    Arc::new(tokio::sync::Mutex::new(client)),
                ));
            }
            Err(error) => println!("{} MCP {} 连接失败：{error}", "✘".red(), config.name),
        }
    }
    clients
}

fn mcp_config_path(root: &std::path::Path) -> std::path::PathBuf {
    root.join("mcp-servers.json")
}

fn load_mcp_configs(root: &std::path::Path) -> Vec<McpServerConfig> {
    std::fs::read_to_string(mcp_config_path(root))
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

fn save_mcp_configs(root: &std::path::Path, configs: &[McpServerConfig]) {
    if let Ok(content) = serde_json::to_string_pretty(configs) {
        let _ = std::fs::write(mcp_config_path(root), content);
    }
}

fn data_root(override_dir: Option<PathBuf>) -> PathBuf {
    override_dir
        .or_else(|| std::env::var("OWO_AGENT_DATA").ok().map(PathBuf::from))
        .unwrap_or_else(|| {
            std::env::var("LOCALAPPDATA")
                .map(|dir| PathBuf::from(dir).join("OwO").join("Agent"))
                .unwrap_or_else(|_| PathBuf::from("data/agent"))
        })
}

/// 优先使用默认数据目录；不可写时回退到工作区 `.owo-agent/`。
fn ensure_data_root(override_dir: Option<PathBuf>, workspace: &std::path::Path) -> PathBuf {
    let preferred = data_root(override_dir);
    if std::fs::create_dir_all(&preferred).is_ok() {
        return preferred;
    }
    let fallback = workspace.join(".owo-agent");
    let _ = std::fs::create_dir_all(&fallback);
    fallback
}

fn display_path(path: &std::path::Path) -> String {
    let raw = path.to_string_lossy();
    raw.strip_prefix(r"\\?\").unwrap_or(&raw).to_string()
}

async fn run_turn(args: TurnArgs) -> Result<(), Box<dyn std::error::Error>> {
    let workspace = args.workspace.canonicalize()?;
    let settings = Settings::load(&workspace);
    apply_egress_setting(&settings);
    settings.apply_usage_env();
    let model = resolve_model(args.model, settings.model.as_deref());
    let agent = build_agent(&workspace, &model, false)?;
    let mut session = Session::new(workspace.clone(), model, None);
    let abort = Arc::new(AtomicBool::new(false));
    let abort_flag = Arc::clone(&abort);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            abort_flag.store(true, Ordering::Relaxed);
        }
    });

    let approver: Arc<dyn Approver> = if args.no_approval {
        Arc::new(AutoApprover { allow: true })
    } else {
        Arc::new(ConsoleApprover {
            stdin: SharedStdin::new(),
        })
    };

    let mut printer = EventPrinter::new();
    let mut on_event = |event: &TurnEvent| printer.print(event);
    let outcome = agent
        .run_turn(
            &mut session,
            &args.prompt,
            approver.as_ref(),
            &abort,
            &mut on_event,
        )
        .await?;
    println!(
        "\n[完成] 工具步数：{}，最终文本：{}",
        outcome.steps,
        outcome.final_text.is_some()
    );

    let diffs = session.diff();
    if !diffs.is_empty() {
        println!("[diff] 本次会话改动文件：");
        for diff in diffs {
            println!("  - {}", diff.path);
        }
    }
    let audit = agent.audit_log();
    if let Ok(audit) = audit.lock() {
        println!("[审计] 记录 {} 条", audit.entries.len());
    }
    let root = ensure_data_root(None, &workspace);
    let trace = TraceRecord::from_outcome(&session, &outcome);
    if let Ok(path) = save_trace(&root.join("traces"), &trace) {
        println!("[trace] {}", display_path(&path));
    }
    let audit_entries = agent
        .audit_log()
        .lock()
        .map(|guard| guard.entries.clone())
        .unwrap_or_default();
    if let Ok(store) = SqliteSessionStore::open(&root.join("index.db")) {
        let _ = store.append_audit(&audit_entries);
    }
    Ok(())
}

async fn run_serve(args: ServeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let workspace = args.workspace.canonicalize()?;
    let settings = Settings::load(&workspace);
    apply_egress_setting(&settings);
    settings.apply_usage_env();
    let model = resolve_model(None, settings.model.as_deref());
    let root = ensure_data_root(None, &workspace);
    let plugin_state = owo_agent_core::PluginStateStore::new(Some(root.join("plugin_state.json")));
    let plugins =
        owo_agent_core::plugin::discover_enabled_plugins(&workspace, &root, &plugin_state);
    let mut mcp_configs = load_mcp_configs(&root);
    merge_plugin_mcp(&plugins, &mut mcp_configs);
    let mcp_clients = connect_mcp_clients(&mcp_configs).await;
    let _ = install_builtin_packages(&builtin_skills_root(), &root);
    let mut skills = SkillRegistry::discover(&workspace, &root);
    apply_disabled_skills(&mut skills, &settings);
    let agent = build_agent_with_mcp(
        &workspace,
        &model,
        settings.read_only,
        &mcp_clients,
        &skills,
        &settings.deny_commands,
    )?;
    let store = SqliteSessionStore::open(&root.join("index.db"))?;
    let state = Arc::new(owo_agent_server::AppState::new(
        agent,
        store,
        root.join("traces"),
        root.clone(),
        workspace.clone(),
    ));
    // R8：强杀恢复——陈旧 pid 文件清理；检测到存活实例则显式拒绝双开。
    if let Some(recovery) = owo_agent_server::shutdown::recover_force_kill(&root)? {
        tracing::warn!(
            "检测到强杀残留（pid={:?}），已清理 pid 文件并恢复干净状态",
            recovery.stale_pid
        );
    }
    let _pid_file = owo_agent_server::shutdown::PidFile::create(&root)?;
    // R8：优雅关闭接线——停止接收 → 完成在途 → flush 审计 → 清理 pid → 退出。
    let shutdown_state = Arc::clone(&state);
    let shutdown_root = root.clone();
    tokio::spawn(async move {
        let gate = Arc::clone(&shutdown_state.shutdown_gate);
        gate.wait_shutdown_request().await;
        tracing::info!("收到关闭请求：等待在途回合完成（上限 30s）");
        let remaining = gate.await_drain(std::time::Duration::from_secs(30)).await;
        if remaining > 0 {
            tracing::warn!("仍有 {remaining} 个在途回合超时未完成，强制执行退出");
        }
        owo_agent_server::flush_audit(&shutdown_state);
        // process::exit 不执行 Drop，显式清理 pid 文件（强杀残留仍由 recover_force_kill 兜底）。
        let _ = std::fs::remove_file(shutdown_root.join("server.pid"));
        tracing::info!("审计已 flush，服务退出");
        std::process::exit(0);
    });
    // 启动时同步插件禁用状态到 Agent 工具前缀（热卸载重启后仍生效）。
    if let Ok(plugin_state) = state.plugin_state.lock() {
        for id in plugin_state.disabled_ids() {
            let prefix = owo_agent_core::tools::mcp_tool_prefix(&id);
            state.agent.set_tool_prefix_enabled(&prefix, false);
        }
    }
    let observer_state = Arc::clone(&state);
    tokio::spawn(async move {
        owo_agent_server::start_observer(observer_state).await;
    });
    let automation_state = Arc::clone(&state);
    tokio::spawn(async move {
        owo_agent_server::start_automation_loop(automation_state).await;
    });
    let memory_state = Arc::clone(&state);
    tokio::spawn(async move {
        owo_agent_server::start_memory_observer(memory_state).await;
    });
    let app = owo_agent_server::build_router(Arc::clone(&state));
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], args.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("owo-agent server listening on http://{addr}");
    let result = axum::serve(listener, app).await;
    // 服务退出：终止全部 MCP stdio 子进程，不留孤儿进程。
    let shutdown_errors = state.agent.shutdown_all_mcp().await;
    for (name, error) in shutdown_errors {
        tracing::warn!("MCP 服务器 {name} 关闭失败：{error}");
    }
    result?;
    Ok(())
}

fn merge_plugin_mcp(
    plugins: &[(std::path::PathBuf, PluginManifest)],
    configs: &mut Vec<McpServerConfig>,
) {
    for (manifest_path, manifest) in plugins {
        if let Some(config) = owo_agent_core::plugin_mcp_config(manifest_path, manifest) {
            if !configs.iter().any(|existing| existing.name == config.name) {
                configs.push(config);
            }
        }
    }
}

fn run_init(args: InitArgs) -> Result<(), Box<dyn std::error::Error>> {
    let workspace = args.workspace.canonicalize()?;
    let target = workspace.join("AGENTS.md");
    if target.exists() && !args.force {
        println!("{} {}", "AGENTS.md 已存在".yellow(), target.display());
        println!("如需覆盖请使用 --force");
        return Ok(());
    }
    std::fs::write(&target, AGENTS_TEMPLATE)?;
    println!("{} {}", "已生成".green(), target.display());
    Ok(())
}

fn print_event(event: &TurnEvent) {
    match event {
        TurnEvent::ModelCall => println!("{}", "  ↻ 调用模型…".cyan()),
        TurnEvent::PermissionRequest(request) => println!(
            "  {} 需要 {} 权限：{}（{}）",
            "审批".yellow(),
            request.level.label(),
            request.tool,
            request.reason
        ),
        TurnEvent::ToolStart { tool, .. } => {
            println!("  {} {tool} …", "▶".blue());
        }
        TurnEvent::ToolResult {
            tool, ok, error, ..
        } => {
            if *ok {
                println!("  {} {tool}", "✔".green());
            } else {
                println!(
                    "  {} {tool}：{}",
                    "✘".red(),
                    error.as_deref().unwrap_or("未知错误")
                );
            }
        }
        TurnEvent::TokenDelta { .. } => {}
        TurnEvent::Compaction { summary } => {
            println!("  {}（上下文已压缩：{}）", "✦".yellow(), summary);
        }
        TurnEvent::Final { .. } => {}
    }
}

/// 事件打印器：把流式增量逐字输出，Final 只收尾不重复打印。
struct EventPrinter {
    streamed: bool,
}

impl EventPrinter {
    fn new() -> Self {
        Self { streamed: false }
    }

    fn print(&mut self, event: &TurnEvent) {
        match event {
            TurnEvent::TokenDelta { delta } => {
                use std::io::Write;
                self.streamed = true;
                print!("{delta}");
                let _ = std::io::stdout().flush();
            }
            TurnEvent::Final { text } => {
                if self.streamed {
                    println!();
                    self.streamed = false;
                } else {
                    println!("\n{}\n{text}", "── 结果 ──".bold());
                }
            }
            other => print_event(other),
        }
    }
}

struct Repl {
    workspace: PathBuf,
    model: String,
    read_only: bool,
    no_approval: bool,
    data_root: PathBuf,
    store: SqliteSessionStore,
    session: Option<Session>,
    agent: Arc<Agent>,
    abort: Arc<AtomicBool>,
    stdin: SharedStdin,
    mcp_configs: Vec<McpServerConfig>,
    mcp_clients: Vec<(String, Arc<tokio::sync::Mutex<McpClient>>)>,
    skills: SkillRegistry,
    settings: Settings,
    plugins: Vec<PluginManifest>,
    audit_flushed: usize,
    perception: SituationStore,
    learn: LearnRecorder,
    whitelist: Whitelist,
    proactive: ProactiveEngine,
}

impl Repl {
    async fn run(args: ReplArgs) -> Result<(), Box<dyn std::error::Error>> {
        let workspace = args.workspace.canonicalize()?;
        let settings = Settings::load(&workspace);
        apply_egress_setting(&settings);
        settings.apply_usage_env();
        let model = resolve_model(args.model, settings.model.as_deref());
        let read_only = args.agent == "plan" || settings.read_only;
        let root = ensure_data_root(args.data_dir, &workspace);
        let store = SqliteSessionStore::open(&root.join("index.db"))?;
        let mut mcp_configs = load_mcp_configs(&root);
        for server in settings.mcp_servers.clone() {
            if !mcp_configs.iter().any(|config| config.name == server.name) {
                mcp_configs.push(server);
            }
        }
        let discovered_plugins = discover_plugins(&workspace, &root);
        let plugin_state =
            owo_agent_core::PluginStateStore::new(Some(root.join("plugin_state.json")));
        let enabled_plugins =
            owo_agent_core::plugin::discover_enabled_plugins(&workspace, &root, &plugin_state);
        merge_plugin_mcp(&enabled_plugins, &mut mcp_configs);
        let plugins: Vec<PluginManifest> = discovered_plugins
            .into_iter()
            .map(|(_, manifest)| manifest)
            .collect();
        let mcp_clients = connect_mcp_clients(&mcp_configs).await;
        let _ = install_builtin_packages(&builtin_skills_root(), &root);
        let mut skills = SkillRegistry::discover(&workspace, &root);
        apply_disabled_skills(&mut skills, &settings);
        let mut whitelist = Whitelist::default();
        for entry in settings.whitelist.clone() {
            whitelist.upsert(entry);
        }
        let proactive = ProactiveEngine::new(settings.proactive.clone());
        let agent = Arc::new(build_agent_with_mcp(
            &workspace,
            &model,
            read_only,
            &mcp_clients,
            &skills,
            &settings.deny_commands,
        )?);
        let mut repl = Repl {
            workspace,
            model,
            read_only,
            no_approval: args.no_approval,
            data_root: root.clone(),
            store,
            session: None,
            agent,
            abort: Arc::new(AtomicBool::new(false)),
            stdin: SharedStdin::new(),
            mcp_configs,
            mcp_clients,
            skills,
            settings,
            plugins,
            audit_flushed: 0,
            perception: SituationStore::new(),
            learn: LearnRecorder::new(),
            whitelist,
            proactive,
        };

        println!(
            "{} {}（{}）",
            "OwO Agent".bold(),
            env!("CARGO_PKG_VERSION").cyan(),
            display_path(&repl.workspace).dimmed()
        );
        println!("输入 /help 查看命令；直接输入文字开始任务。");

        let history_path = root.join("history.txt");
        if std::io::stdin().is_terminal() {
            repl.run_terminal(&history_path).await?;
        } else {
            repl.run_piped().await?;
        }
        if let Some(session) = &repl.session {
            let _ = repl.store.save(session);
        }
        Ok(())
    }

    async fn run_terminal(
        &mut self,
        history_path: &std::path::Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut rl = rustyline::DefaultEditor::new()?;
        if let Ok(content) = std::fs::read_to_string(history_path) {
            for line in content.lines() {
                let _ = rl.add_history_entry(line);
            }
        }
        loop {
            let prompt = if self.read_only {
                format!("{} ", "plan ❯".yellow())
            } else {
                format!("{} ", "build ❯".green())
            };
            match rl.readline(&prompt) {
                Ok(line) => {
                    let line = line.trim().to_string();
                    if line.is_empty() {
                        continue;
                    }
                    let _ = rl.add_history_entry(line.clone());
                    match self.handle_line(&line).await {
                        Ok(true) => break,
                        Ok(false) => continue,
                        Err(error) => eprintln!("错误：{error}"),
                    }
                }
                Err(ReadlineError::Interrupted) => {
                    println!("{}", "（/exit 退出；已取消当前输入）".dimmed());
                    continue;
                }
                Err(ReadlineError::Eof) => break,
                Err(error) => {
                    eprintln!("输入错误：{error}");
                    break;
                }
            }
        }
        let history: Vec<String> = rl.history().iter().cloned().collect();
        let _ = std::fs::write(history_path, history.join("\n"));
        Ok(())
    }

    async fn run_piped(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let mut line = String::new();
        loop {
            print!(
                "{} ",
                if self.read_only {
                    "plan ❯".yellow()
                } else {
                    "build ❯".green()
                }
            );
            use std::io::Write;
            let _ = std::io::stdout().flush();
            line.clear();
            let read = self.stdin.read_line(&mut line).await?;
            if read == 0 {
                break;
            }
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            match self.handle_line(&line).await {
                Ok(true) => break,
                Ok(false) => continue,
                Err(error) => eprintln!("错误：{error}"),
            }
        }
        Ok(())
    }

    async fn handle_line(&mut self, line: &str) -> Result<bool, Box<dyn std::error::Error>> {
        if let Some(query) = line.strip_prefix("@explore ") {
            self.run_at_subagent(query, true).await?;
            return Ok(false);
        }
        if let Some(task) = line.strip_prefix("@subagent ") {
            self.run_at_subagent(task, false).await?;
            return Ok(false);
        }
        if let Some(command) = line.strip_prefix('/') {
            let mut parts = command.split_whitespace();
            match parts.next().unwrap_or_default() {
                "help" => print_help(),
                "exit" | "quit" => return Ok(true),
                "new" => self.new_session(parts.next()).await?,
                "sessions" => self.list_sessions(),
                "resume" => {
                    let id = parts
                        .next()
                        .ok_or("用法：/resume <会话ID>（/sessions 查看）")?;
                    self.resume(id).await?;
                }
                "model" => match parts.next() {
                    Some(model) => {
                        self.model = model.to_string();
                        self.rebuild_agent()?;
                        println!("{} {}", "模型已切换：".green(), self.model);
                    }
                    None => println!("当前模型：{}", self.model),
                },
                "plan" => {
                    self.set_mode(true)?;
                }
                "build" => {
                    self.set_mode(false)?;
                }
                "agent" => match parts.next() {
                    Some("plan") => self.set_mode(true)?,
                    Some("build") => self.set_mode(false)?,
                    Some(other) => println!("未知 agent：{other}（build / plan）"),
                    None => println!(
                        "当前 agent：{}",
                        if self.read_only {
                            "plan（只读）"
                        } else {
                            "build"
                        }
                    ),
                },
                "diff" => self.show_diff(),
                "undo" | "revert" => self.undo().await?,
                "mcp" => self.handle_mcp(command).await?,
                "skills" => match parts.next() {
                    Some("reload") => {
                        self.reload_skills()?;
                    }
                    _ => self.list_skills(),
                },
                "fork" => self.fork_session(parts.next()).await?,
                "rewind" => {
                    let keep = parts.next().ok_or("用法：/rewind <保留消息数>")?;
                    self.rewind_session(keep).await?;
                }
                "redo" => self.redo_session().await?,
                "undo-msg" => self.undo_message(parts.next()).await?,
                "redo-msg" => self.redo_message().await?,
                "tree" => self.show_tree(),
                "share" => self.share_session(parts.next())?,
                "traces" => self.list_traces(),
                "trace" => self.show_trace(parts.next())?,
                "settings" => self.show_settings(),
                "plugins" => self.list_plugins(),
                "whitelist" => self.show_whitelist(),
                "perception" => self.show_perception(),
                "learn" => self.handle_learn(command)?,
                "proactive" => self.handle_proactive(command)?,
                "status" => self.show_status(),
                "permissions" => self.show_permissions(),
                "audit" => self.show_audit(),
                "init" => {
                    let target = self.workspace.join("AGENTS.md");
                    if target.exists() {
                        println!("{} {}", "AGENTS.md 已存在：".yellow(), target.display());
                    } else {
                        std::fs::write(&target, AGENTS_TEMPLATE)?;
                        println!("{} {}", "已生成".green(), target.display());
                    }
                }
                "abort" => {
                    self.abort.store(true, Ordering::Relaxed);
                    println!("已请求中止当前回合");
                }
                "clear" => print!("\x1b[2J\x1b[1;1H"),
                other => println!("未知命令：/{other}（/help 查看全部）"),
            }
            return Ok(false);
        }
        self.run_turn(line).await?;
        Ok(false)
    }

    fn set_mode(&mut self, read_only: bool) -> Result<(), Box<dyn std::error::Error>> {
        self.read_only = read_only;
        self.rebuild_agent()?;
        println!(
            "{}",
            if read_only {
                "已切换到 plan 模式（只读，写/执行将被拒绝）".yellow()
            } else {
                "已切换到 build 模式（写/执行需要审批）".green()
            }
        );
        Ok(())
    }

    fn rebuild_agent(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.audit_flushed = 0;
        self.agent = Arc::new(build_agent_with_mcp(
            &self.workspace,
            &self.model,
            self.read_only,
            &self.mcp_clients,
            &self.skills,
            &self.settings.deny_commands,
        )?);
        Ok(())
    }

    fn list_skills(&self) {
        let skills = self.agent.skills().list();
        if skills.is_empty() {
            println!(
                "暂无技能（放置到 {}/skills 或 .agents/skills/，每技能一个含 SKILL.md 的目录）",
                display_path(&self.data_root)
            );
            return;
        }
        for skill in skills {
            let marker = if self.skills.is_enabled(&skill.name) {
                String::new()
            } else {
                " [禁用]".dimmed().to_string()
            };
            println!("{}：{}{}", skill.name.cyan(), skill.description, marker);
        }
    }

    fn reload_skills(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let mut skills = SkillRegistry::discover(&self.workspace, &self.data_root);
        apply_disabled_skills(&mut skills, &self.settings);
        self.skills = skills;
        self.rebuild_agent()?;
        println!(
            "{} 已重新加载 {} 个技能",
            "✓".green(),
            self.skills.list().len()
        );
        Ok(())
    }

    async fn handle_mcp(&mut self, command: &str) -> Result<(), Box<dyn std::error::Error>> {
        let rest = command.trim_start_matches("mcp").trim_start().to_string();
        let mut parts = rest.split_whitespace();
        match parts.next() {
            Some("add") => {
                let name = parts
                    .next()
                    .ok_or("用法：/mcp add <名称> <命令> [参数...]")?
                    .to_string();
                let command_line: Vec<&str> = parts.collect();
                let config = if matches!(command_line.first().copied(), Some("http" | "https")) {
                    let url = command_line
                        .get(1)
                        .copied()
                        .ok_or("HTTP MCP 用法：/mcp add <名称> http <URL>")?;
                    McpServerConfig::http(&name, url)
                } else {
                    let command = command_line
                        .first()
                        .ok_or("缺少 MCP 服务器命令（如 npx、node、python）")?;
                    McpServerConfig::stdio(
                        &name,
                        *command,
                        command_line[1..]
                            .iter()
                            .map(|arg| arg.to_string())
                            .collect(),
                    )
                };
                match McpClient::connect(&config).await {
                    Ok(client) => {
                        let tool_count = client.tools().len();
                        self.mcp_clients
                            .push((name.clone(), Arc::new(tokio::sync::Mutex::new(client))));
                        self.mcp_configs.push(config);
                        save_mcp_configs(&self.data_root, &self.mcp_configs);
                        self.rebuild_agent()?;
                        println!("{} MCP {name}（{tool_count} 个工具）", "已添加".green());
                    }
                    Err(error) => println!("{} 连接失败：{error}", "✘".red()),
                }
            }
            Some("list") => {
                if self.mcp_clients.is_empty() {
                    println!("未配置 MCP 服务器（/mcp add <名称> <命令>）");
                } else {
                    for (name, client) in &self.mcp_clients {
                        let transport = self
                            .mcp_configs
                            .iter()
                            .find(|config| config.name == *name)
                            .map(|config| config.transport.as_str())
                            .unwrap_or("stdio");
                        let tool_names = match client.try_lock() {
                            Ok(guard) => guard
                                .tools()
                                .into_iter()
                                .map(|tool| tool.name)
                                .collect::<Vec<_>>()
                                .join(", "),
                            Err(_) => "（忙碌）".to_string(),
                        };
                        println!("{name}（{transport}）：{tool_names}");
                    }
                }
            }
            Some("remove") => {
                let name = parts.next().ok_or("用法：/mcp remove <名称>")?;
                if let Some(position) = self
                    .mcp_clients
                    .iter()
                    .position(|(existing, _)| existing == name)
                {
                    let (_, client) = self.mcp_clients.remove(position);
                    let _ = client.lock().await.shutdown().await;
                }
                self.mcp_configs.retain(|config| config.name != name);
                save_mcp_configs(&self.data_root, &self.mcp_configs);
                self.rebuild_agent()?;
                println!("已移除 MCP 服务器：{name}");
            }
            _ => {
                println!("用法：/mcp add <名称> <命令> [参数...] | /mcp list | /mcp remove <名称>")
            }
        }
        Ok(())
    }

    async fn new_session(&mut self, model: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(session) = &self.session {
            self.store.save(session)?;
        }
        let model = model
            .map(str::to_string)
            .unwrap_or_else(|| self.model.clone());
        self.model = model.clone();
        let session = self.store.create(&self.workspace, &model, None)?;
        println!("{} {}", "新会话：".green(), session.id.dimmed());
        self.session = Some(session);
        Ok(())
    }

    async fn fork_session(
        &mut self,
        index: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(current) = &self.session else {
            println!("暂无会话");
            return Ok(());
        };
        let index = match index {
            Some(value) => value.parse().map_err(|_| "消息序号需为数字".to_string())?,
            None => current.messages.len().saturating_sub(1),
        };
        let child = current.fork(index);
        self.store.save(&child)?;
        let id = child.id.clone();
        self.session = Some(child);
        println!("{} 子会话 {id}（在消息 {index} 处 fork）", "已创建".green());
        Ok(())
    }

    async fn rewind_session(&mut self, keep: &str) -> Result<(), Box<dyn std::error::Error>> {
        let Some(session) = &mut self.session else {
            println!("暂无会话");
            return Ok(());
        };
        let keep: usize = keep.parse().map_err(|_| "保留消息数需为数字".to_string())?;
        if keep < session.messages.len() {
            session.revert().await?;
        }
        let removed = session.rewind(keep);
        self.store.save(session)?;
        println!(
            "{} 已回退到 {keep} 条消息（移除 {} 条，/redo 可恢复）",
            "↶".yellow(),
            removed.len()
        );
        Ok(())
    }

    async fn redo_session(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let Some(session) = &mut self.session else {
            println!("暂无会话");
            return Ok(());
        };
        let restored = session.redo().map(|tail| tail.len()).unwrap_or(0);
        self.store.save(session)?;
        if restored == 0 {
            println!("没有可恢复的历史");
        } else {
            println!("{} 已恢复 {restored} 条消息", "↷".green());
        }
        Ok(())
    }

    async fn undo_message(
        &mut self,
        count: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(session) = &mut self.session else {
            println!("暂无会话");
            return Ok(());
        };
        let count = match count {
            Some(value) => value.parse().map_err(|_| "数量需为数字".to_string())?,
            None => 1,
        };
        let removed = session.undo_message(count);
        match removed {
            Some(messages) => {
                self.store.save(session)?;
                println!(
                    "{} 已撤销 {} 条消息（/redo-msg 恢复）",
                    "↶".yellow(),
                    messages.len()
                );
            }
            None => println!("没有可撤销的消息"),
        }
        Ok(())
    }

    async fn redo_message(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let Some(session) = &mut self.session else {
            println!("暂无会话");
            return Ok(());
        };
        let restored = session.redo_message().map(|tail| tail.len()).unwrap_or(0);
        self.store.save(session)?;
        if restored == 0 {
            println!("没有可恢复的消息");
        } else {
            println!("{} 已恢复 {restored} 条消息", "↷".green());
        }
        Ok(())
    }

    fn show_tree(&self) {
        let ids = self.store.list();
        if ids.is_empty() {
            println!("暂无会话");
            return;
        }
        for id in ids {
            if let Ok(session) = self.store.load(&id) {
                let active = self
                    .session
                    .as_ref()
                    .map(|current| current.id == id)
                    .unwrap_or(false);
                let parent = session.parent_id.as_deref().unwrap_or("(根)");
                println!(
                    "{} {} parent={} fork={} msgs={}",
                    if active {
                        "▶".green().to_string()
                    } else {
                        "  ".to_string()
                    },
                    id,
                    parent,
                    session
                        .fork_point
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "-".into()),
                    session.messages.len()
                );
            }
        }
    }

    fn share_session(&self, format: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let Some(session) = &self.session else {
            println!("暂无会话");
            return Ok(());
        };
        let format = format.unwrap_or("md");
        let shares = self.data_root.join("shares");
        std::fs::create_dir_all(&shares)?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let path = match format {
            "html" => shares.join(format!("{}-{stamp}.html", session.id)),
            _ => shares.join(format!("{}-{stamp}.md", session.id)),
        };
        let content = match format {
            "html" => export_html(session),
            _ => export_markdown(session),
        };
        std::fs::write(&path, content)?;
        println!("{} 会话已导出：{}", "已分享".green(), display_path(&path));
        Ok(())
    }

    fn list_traces(&self) {
        let traces = list_traces(&self.data_root.join("traces"));
        if traces.is_empty() {
            println!("暂无 trace（完成回合后自动记录）");
            return;
        }
        for (index, path) in traces.iter().enumerate() {
            if let Ok(trace) = load_trace(path) {
                let preview: String = trace.prompt.chars().take(40).collect();
                println!(
                    "{}: {} steps={} {}ms final={}",
                    index,
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    trace.steps,
                    trace.duration_ms,
                    trace.final_text.is_some()
                );
                println!("      {}", preview.dimmed());
            }
        }
    }

    fn show_trace(&self, index: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let index: usize = index
            .ok_or("用法：/trace <序号>（/traces 查看）")?
            .parse()?;
        let traces = list_traces(&self.data_root.join("traces"));
        let path = traces
            .get(index)
            .ok_or_else(|| format!("trace 序号越界（共 {} 条）", traces.len()))?;
        let trace = load_trace(path)?;
        println!("{}", serde_json::to_string_pretty(&trace)?);
        Ok(())
    }

    fn show_settings(&self) {
        println!(
            "{}",
            serde_json::to_string_pretty(&self.settings).unwrap_or_default()
        );
    }

    fn list_plugins(&self) {
        if self.plugins.is_empty() {
            println!("未加载插件（放置到 <workspace>/.owo/plugins/ 或 <data>/plugins/，每插件一个含 manifest.json 的目录）");
            return;
        }
        for manifest in &self.plugins {
            let tool_count = self
                .mcp_clients
                .iter()
                .find(|(name, _)| name == &manifest.id)
                .and_then(|(_, client)| client.try_lock().ok())
                .map(|guard| guard.tools().len())
                .unwrap_or(0);
            println!(
                "{} v{}（{}）——{}",
                manifest.name,
                manifest.version,
                manifest.id,
                if manifest.description.is_empty() {
                    tool_count.to_string() + " 个工具"
                } else {
                    format!("{}，{} 个工具", manifest.description, tool_count)
                }
            );
        }
    }

    fn list_sessions(&self) {
        let ids = self.store.list();
        if ids.is_empty() {
            println!("暂无会话（/new 创建）");
            return;
        }
        for id in ids {
            match self.store.load(&id) {
                Ok(session) => {
                    let active = self
                        .session
                        .as_ref()
                        .map(|s| s.id == session.id)
                        .unwrap_or(false);
                    let mut badges = String::new();
                    if session.pinned {
                        badges.push_str(" 📌");
                    }
                    if session.archived {
                        badges.push_str(" 🗄");
                    }
                    let short_id: String = id.chars().take(8).collect();
                    println!(
                        "{} {}{}  {}  model={}  msgs={}  updated={}",
                        if active {
                            "▶".green().to_string()
                        } else {
                            "  ".to_string()
                        },
                        session.display_title(),
                        badges,
                        short_id.dimmed(),
                        session.model,
                        session.messages.len(),
                        session.updated_at,
                    );
                }
                Err(error) => println!("{} {}（{error}）", "损坏会话：".red(), id),
            }
        }
    }

    async fn resume(&mut self, id: &str) -> Result<(), Box<dyn std::error::Error>> {
        let session = self.store.load(id)?;
        if session.workspace != self.workspace {
            println!(
                "{} 会话工作区 {} 与当前 {} 不同",
                "警告：".yellow(),
                display_path(&session.workspace),
                display_path(&self.workspace)
            );
        }
        self.session = Some(session);
        println!("{}", format!("已恢复会话：{id}").green());
        Ok(())
    }

    fn show_diff(&self) {
        let Some(session) = &self.session else {
            println!("暂无会话");
            return;
        };
        let diffs = session.diff();
        if diffs.is_empty() {
            println!("{}", "当前会话没有未回滚的文件改动".dimmed());
            return;
        }
        for diff in diffs {
            println!("{} {}", "●".cyan(), diff.path.bold());
            if let Some(before) = &diff.before {
                for line in before.lines() {
                    println!("{} {}", "-".red(), line);
                }
            } else {
                println!("{}", "(新建文件)".green());
            }
            if let Some(after) = &diff.after {
                for line in after.lines() {
                    println!("{} {}", "+".green(), line);
                }
            } else {
                println!("{}", "(已删除)".red());
            }
        }
    }

    async fn undo(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let Some(session) = &mut self.session else {
            println!("暂无会话");
            return Ok(());
        };
        let restored = session.revert().await?;
        if restored.is_empty() {
            println!("{}", "没有可回滚的改动".dimmed());
        } else {
            println!("{} {}", "已回滚：".green(), restored.join(", "));
        }
        self.store.save(session)?;
        Ok(())
    }

    fn show_status(&self) {
        println!("工作区：{}", display_path(&self.workspace));
        println!("模型：{}", self.model);
        println!(
            "模式：{}",
            if self.read_only {
                "plan（只读）".yellow()
            } else {
                "build".green()
            }
        );
        match &self.session {
            Some(session) => {
                println!("会话：{}", session.id);
                println!("消息数：{}", session.messages.len());
                println!("未回滚改动：{}", session.diff().len());
            }
            None => println!("会话：{}", "无（输入任务时自动创建）".dimmed()),
        }
        let audit_count = match self.agent.audit_log().lock() {
            Ok(guard) => guard.entries.len(),
            Err(_) => 0,
        };
        println!("审计记录：{audit_count} 条");
    }

    fn show_permissions(&self) {
        println!("{}", "权限策略：deny 优先 → allow 规则 → ask 审批".bold());
        println!("  read（read_file/list_dir/search_files）：作用域内自动放行");
        println!("  write（write_file）：默认审批，工作区外拒绝，可 /undo 回滚");
        println!("  execute（run_command）：默认审批，危险命令直接拒绝，60s 超时");
        if self.read_only {
            println!("{}", "当前 plan 模式：写/执行/注入一律拒绝".yellow());
        }
    }

    fn show_audit(&self) {
        let audit = self.agent.audit_log();
        let Ok(audit) = audit.lock() else {
            return;
        };
        if audit.entries.is_empty() {
            println!("暂无审计记录");
            return;
        }
        for entry in audit.entries.iter().rev().take(20) {
            let tag = match entry.event.as_str() {
                "permission" => format!(
                    "[审批 {}]",
                    entry.approved.map(|v| v.to_string()).unwrap_or_default()
                ),
                "tool_call" => "[工具]".to_string(),
                _ => format!("[{}]", entry.event),
            };
            println!(
                "{} {} {} {}",
                tag.dimmed(),
                entry.tool.as_deref().unwrap_or("").cyan(),
                entry.detail.dimmed(),
                entry.ts.dimmed()
            );
        }
    }

    fn show_whitelist(&self) {
        println!(
            "{}",
            format!("应用白名单（{} 项）：", self.whitelist.entries().len()).bold()
        );
        for entry in self.whitelist.entries() {
            println!(
                "  {} {}（{}）操作={} 学习={} 敏感={}",
                entry.app_id,
                entry.name,
                entry.tier.label(),
                if entry.auto_ops_allowed {
                    "允许".green()
                } else {
                    "禁止".red()
                },
                if entry.learn_allowed {
                    "允许".green()
                } else {
                    "禁止".red()
                },
                entry.sensitive
            );
        }
    }

    fn show_perception(&self) {
        let snapshot = self.perception.snapshot();
        println!("{}", "当前情景快照（按权限过滤）：".bold());
        println!(
            "  {}",
            serde_json::to_string_pretty(&snapshot).unwrap_or_default()
        );
    }

    fn handle_learn(&mut self, command: &str) -> Result<(), Box<dyn std::error::Error>> {
        let mut parts = command.split_whitespace();
        parts.next(); // "learn"
        match parts.next() {
            Some("start") => {
                self.learn.start();
                println!(
                    "{}",
                    "开始录制示范操作（Ctrl+C 前的操作会保留在当前样本）".green()
                );
            }
            Some("pause") => {
                self.learn.pause();
                println!("录制已暂停");
            }
            Some("resume") => {
                self.learn.resume();
                println!("录制已恢复");
            }
            Some("stop") => {
                let samples = self.learn.stop();
                println!("录制结束，共 {} 条动作样本", samples.len());
            }
            Some("clear") => {
                self.learn.clear();
                println!("已清空本次样本");
            }
            Some("status") | None => {
                println!(
                    "状态：{:?} ｜ 样本：{} ｜ 敏感面熔断：{}",
                    self.learn.state(),
                    self.learn.samples(),
                    self.learn.sensitive_break()
                );
            }
            Some(other) => println!("未知子命令：{other}（start/pause/resume/stop/clear/status）"),
        }
        Ok(())
    }

    fn handle_proactive(&mut self, command: &str) -> Result<(), Box<dyn std::error::Error>> {
        let mut parts = command.split_whitespace();
        parts.next(); // "proactive"
        match parts.next() {
            Some("status") | None => {
                let suggestions = self.proactive.suggestions();
                if suggestions.is_empty() {
                    println!("暂无主动建议（默认仅提示，不执行）");
                } else {
                    for suggestion in suggestions {
                        println!(
                            "  [{}] {} ｜ {}",
                            suggestion.id, suggestion.app_id, suggestion.summary
                        );
                    }
                }
            }
            Some("observe") => {
                let app_id = parts
                    .next()
                    .ok_or("用法：/proactive observe <应用ID> <动作序列>")?;
                let actions: Vec<String> = parts
                    .flat_map(|part| part.split(','))
                    .map(|action| action.trim().to_string())
                    .filter(|action| !action.is_empty())
                    .collect();
                if actions.is_empty() {
                    return Err("动作序列不能为空".into());
                }
                match self.proactive.observe(app_id, actions) {
                    Some(suggestion) => println!(
                        "{} [{}] {}",
                        "建议：".yellow(),
                        suggestion.id,
                        suggestion.summary
                    ),
                    None => println!("未达到建议阈值"),
                }
            }
            Some("decide") => {
                let id = parts
                    .next()
                    .ok_or("用法：/proactive decide <建议ID> <learn|execute|ignore|mute>")?;
                let action = match parts.next() {
                    Some("learn") => SuggestionAction::Learn,
                    Some("execute") => SuggestionAction::ExecuteOnce,
                    Some("ignore") => SuggestionAction::Ignore,
                    Some("mute") => SuggestionAction::MuteForever,
                    _ => return Err("动作需为 learn/execute/ignore/mute".into()),
                };
                self.proactive.decide(id, action)?;
                println!("已处理建议 {id}");
            }
            Some(other) => println!("未知子命令：{other}（status/observe/decide）"),
        }
        Ok(())
    }

    async fn run_turn(&mut self, prompt: &str) -> Result<(), Box<dyn std::error::Error>> {
        if self.session.is_none() {
            self.new_session(None).await?;
        }
        let mut session = self.session.take().expect("session just created");
        let prompt = prompt.to_string();
        self.abort.store(false, Ordering::Relaxed);
        let agent = Arc::clone(&self.agent);
        let abort = Arc::clone(&self.abort);
        let approver: Arc<dyn Approver> = if self.no_approval {
            Arc::new(AutoApprover { allow: true })
        } else {
            Arc::new(ConsoleApprover {
                stdin: self.stdin.clone(),
            })
        };

        println!("{} {}", "▶".green(), prompt.dimmed());
        let task = tokio::spawn(async move {
            let mut printer = EventPrinter::new();
            let mut on_event = |event: &TurnEvent| printer.print(event);
            let outcome = agent
                .run_turn(
                    &mut session,
                    &prompt,
                    approver.as_ref(),
                    &abort,
                    &mut on_event,
                )
                .await;
            (outcome, session)
        });

        let abort_flag = Arc::clone(&self.abort);
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                abort_flag.store(true, Ordering::Relaxed);
                println!("{}", "（Ctrl+C：正在中止当前回合…）".yellow());
            }
        });

        let (outcome, session) = task
            .await
            .map_err(|error| std::io::Error::other(format!("回合任务失败：{error}")))?;
        self.session = Some(session);
        if let Some(session) = &self.session {
            self.store.save(session)?;
        }
        self.flush_audit();
        let outcome = outcome?;
        let trace =
            TraceRecord::from_outcome(self.session.as_ref().expect("session saved"), &outcome);
        if let Ok(path) = save_trace(&self.data_root.join("traces"), &trace) {
            println!("[trace] {}", display_path(&path));
        }
        println!(
            "{} 工具步数 {}，审计 {} 条，改动 {} 个文件（/diff 查看，/undo 回滚）",
            "✓".green(),
            outcome.steps,
            self.agent
                .audit_log()
                .lock()
                .map(|log| log.entries.len())
                .unwrap_or(0),
            self.session.as_ref().map(|s| s.diff().len()).unwrap_or(0),
        );
        Ok(())
    }

    fn flush_audit(&mut self) {
        let audit_entries = self
            .agent
            .audit_log()
            .lock()
            .map(|guard| guard.entries.clone())
            .unwrap_or_default();
        if audit_entries.len() <= self.audit_flushed {
            return;
        }
        if self
            .store
            .append_audit(&audit_entries[self.audit_flushed..])
            .is_ok()
        {
            self.audit_flushed = audit_entries.len();
        }
    }

    async fn run_at_subagent(
        &mut self,
        prompt: &str,
        read_only: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let workspace = self.workspace.clone();
        let model = self.model.clone();
        let agent = Arc::clone(&self.agent);
        let text = agent
            .run_subagent(&workspace, &model, prompt, read_only)
            .await?;
        println!(
            "{} {}",
            if read_only {
                "探索结果：".cyan()
            } else {
                "子代理结果：".green()
            },
            text
        );
        Ok(())
    }
}

fn print_help() {
    println!("{}", "── 命令 ──".bold());
    println!("  直接输入文字        向当前 Agent 发起任务");
    println!("  @explore <问题>     直呼只读探索子代理");
    println!("  @subagent <任务>    直呼通用子代理");
    println!("  /new [模型]         新建会话");
    println!("  /sessions           列出会话");
    println!("  /resume <id>        恢复会话");
    println!("  /fork [消息序号]     在指定消息处创建子会话");
    println!("  /rewind <条数>      回退会话历史（文件改动一并撤销）");
    println!("  /redo               恢复最近一次 rewind");
    println!("  /undo-msg [n]       撤销最近 n 条对话消息");
    println!("  /redo-msg           恢复最近一次消息撤销");
    println!("  /tree               查看会话树");
    println!("  /share [html]       导出会话分享（Markdown/HTML）");
    println!("  /traces | /trace <n>  列出/回放回合轨迹");
    println!("  /settings           查看工作区 settings.json 配置");
    println!("  /plugins            列出已加载插件");
    println!("  /model [名称]       查看/切换模型");
    println!("  /plan | /build      切换只读规划模式 / 执行模式");
    println!("  /diff               查看本次会话文件改动");
    println!("  /undo               回滚本次会话全部写操作");
    println!("  /status             查看工作区/模型/会话状态");
    println!("  /permissions        查看权限策略");
    println!("  /audit              查看最近审计记录");
    println!("  /mcp add|list|remove  管理 MCP 服务器");
    println!("  /skills             列出已加载技能");
    println!("  /whitelist          查看应用白名单（v0.4）");
    println!("  /perception         查看当前情景快照（v0.4）");
    println!("  /learn <start|pause|resume|stop|clear|status>  示范学习录制（v0.4）");
    println!("  /proactive <status|observe|decide>  主动建议（v0.4）");
    println!("  /init               生成 AGENTS.md");
    println!("  /abort              中止当前回合");
    println!("  /clear              清屏");
    println!("  /exit | /quit       退出");
}

/// 共享 stdin 读取器：REPL 主循环与审批提示共用同一缓冲，避免管道输入被吞行。
#[derive(Clone)]
struct SharedStdin {
    inner: Arc<tokio::sync::Mutex<tokio::io::BufReader<tokio::io::Stdin>>>,
}

impl SharedStdin {
    fn new() -> Self {
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(tokio::io::BufReader::new(
                tokio::io::stdin(),
            ))),
        }
    }

    async fn read_line(&self, output: &mut String) -> std::io::Result<usize> {
        use tokio::io::AsyncBufReadExt;
        let mut guard = self.inner.lock().await;
        guard.read_line(output).await
    }
}

struct ConsoleApprover {
    stdin: SharedStdin,
}

#[async_trait]
impl Approver for ConsoleApprover {
    async fn decide(&self, request: &PermissionRequest) -> Decision {
        use std::io::Write;
        print!(
            "  {} 允许 {} 执行 {}？[y/N] ",
            "审批".yellow(),
            request.level.label(),
            request.tool
        );
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if self.stdin.read_line(&mut line).await.is_ok() {
            match line.trim().to_lowercase().as_str() {
                "y" | "yes" => return Decision::Allow,
                _ => return Decision::Deny,
            }
        }
        Decision::Deny
    }
}

#[cfg(test)]
mod audit_cli_tests {
    use super::audit_key;
    use super::AuditAction;
    use super::AuditArgs;

    fn args(action: AuditAction, key: Option<String>, key_file: Option<String>) -> AuditArgs {
        AuditArgs {
            action,
            key,
            key_file,
        }
    }

    #[test]
    fn audit_key_from_flag_hex_decodes() {
        let a = args(
            AuditAction::Verify { path: "x".into() },
            Some("00ff10ab".into()),
            None,
        );
        assert_eq!(audit_key(&a).unwrap(), vec![0x00, 0xff, 0x10, 0xab]);
    }

    #[test]
    fn audit_key_missing_is_explicit_error() {
        let a = args(AuditAction::Verify { path: "x".into() }, None, None);
        let err = audit_key(&a).unwrap_err();
        assert!(err.contains("OWO_AUDIT_KEY"), "缺密钥应明确报错：{err}");
    }

    #[test]
    fn audit_key_odd_hex_rejected() {
        let a = args(
            AuditAction::Verify { path: "x".into() },
            Some("abc".into()),
            None,
        );
        assert!(audit_key(&a).is_err());
    }
}
