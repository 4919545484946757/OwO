use async_trait::async_trait;
use clap::{Args, Parser, Subcommand};
use colored::Colorize;
use owo_agent_core::permissions::{Approver, AutoApprover, Decision, PermissionRequest, Policy};
use owo_agent_core::session::{Session, SessionStore};
use owo_agent_core::tools::ToolRegistry;
use owo_agent_core::{
    Agent, AgentConfig, JsonSessionStore, McpClient, McpServerConfig, OpenAiCompatibleConfig,
    OpenAiCompatibleProvider, SkillRegistry, TurnEvent,
};
use rustyline::error::ReadlineError;
use std::future::Future;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

mod tui;

const DEFAULT_MODEL: &str = "gpt-5.1-codex";

const AGENTS_TEMPLATE: &str = r#"# AGENTS.md

<!-- 由 owo-agent /init 生成，按项目实际情况修改。
     该文件会被 Agent 在每次会话开始时注入，作为项目级规则。 -->

## 项目说明

- 一句话描述本项目做什么。

## 开发规则

- 写清楚构建命令、测试命令与代码约定。
- 说明哪些目录/文件禁止修改。
"#;

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
    }
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

fn resolve_model(option: Option<String>) -> String {
    option.unwrap_or_else(|| {
        std::env::var("OPENAI_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string())
    })
}

fn build_agent(
    workspace: &std::path::Path,
    model: &str,
    read_only: bool,
) -> Result<Agent, Box<dyn std::error::Error>> {
    let root = ensure_data_root(None, workspace);
    let skills = SkillRegistry::discover(workspace, &root);
    build_agent_with_mcp(workspace, model, read_only, &[], &skills)
}

fn build_agent_with_mcp(
    workspace: &std::path::Path,
    model: &str,
    read_only: bool,
    mcp_clients: &[(String, Arc<tokio::sync::Mutex<McpClient>>)],
    skills: &SkillRegistry,
) -> Result<Agent, Box<dyn std::error::Error>> {
    let mut config = OpenAiCompatibleConfig::from_env()?;
    config.model = model.to_string();
    let provider = Arc::new(OpenAiCompatibleProvider::new(config)?);
    let policy = if read_only {
        Policy::read_only(workspace.to_path_buf())
    } else {
        Policy::new(workspace.to_path_buf())
    };
    let mut registry = ToolRegistry::new();
    for (server_name, client) in mcp_clients {
        let tools = client
            .try_lock()
            .map_err(|_| format!("MCP 客户端 {server_name} 忙碌"))?
            .tools();
        registry.register_mcp_tools(server_name, Arc::clone(client), tools);
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
    let mut agent = Agent::new(provider, registry, policy, config);
    agent.set_skills(skills.clone());
    Ok(agent)
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
    let model = resolve_model(args.model);
    let agent = build_agent(&workspace, &model, false)?;
    let mut session = Session::new(workspace, model, None);
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
    Ok(())
}

async fn run_serve(args: ServeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let workspace = args.workspace.canonicalize()?;
    let model = resolve_model(None);
    let agent = build_agent(&workspace, &model, false)?;
    let root = ensure_data_root(None, &workspace);
    let store = JsonSessionStore::new(root.join("sessions"));
    let state = Arc::new(owo_agent_server::AppState::new(agent, store));
    let app = owo_agent_server::build_router(state);
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], args.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("owo-agent server listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
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
    store: JsonSessionStore,
    session: Option<Session>,
    agent: Arc<Agent>,
    abort: Arc<AtomicBool>,
    stdin: SharedStdin,
    mcp_configs: Vec<McpServerConfig>,
    mcp_clients: Vec<(String, Arc<tokio::sync::Mutex<McpClient>>)>,
    skills: SkillRegistry,
}

impl Repl {
    async fn run(args: ReplArgs) -> Result<(), Box<dyn std::error::Error>> {
        let workspace = args.workspace.canonicalize()?;
        let model = resolve_model(args.model);
        let read_only = args.agent == "plan";
        let root = ensure_data_root(args.data_dir, &workspace);
        let store = JsonSessionStore::new(root.join("sessions"));
        let mcp_configs = load_mcp_configs(&root);
        let mcp_clients = connect_mcp_clients(&mcp_configs).await;
        let skills = SkillRegistry::discover(&workspace, &root);
        let agent = Arc::new(build_agent_with_mcp(
            &workspace,
            &model,
            read_only,
            &mcp_clients,
            &skills,
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
                "skills" => self.list_skills(),
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
        self.agent = Arc::new(build_agent_with_mcp(
            &self.workspace,
            &self.model,
            self.read_only,
            &self.mcp_clients,
            &self.skills,
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
            println!("{}：{}", skill.name.cyan(), skill.description);
        }
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
                let command = command_line
                    .first()
                    .ok_or("缺少 MCP 服务器命令（如 npx、node、python）")?;
                let config = McpServerConfig {
                    name: name.clone(),
                    command: command.to_string(),
                    args: command_line[1..]
                        .iter()
                        .map(|arg| arg.to_string())
                        .collect(),
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
                        let tool_names = match client.try_lock() {
                            Ok(guard) => guard
                                .tools()
                                .into_iter()
                                .map(|tool| tool.name)
                                .collect::<Vec<_>>()
                                .join(", "),
                            Err(_) => "（忙碌）".to_string(),
                        };
                        println!("{name}：{tool_names}");
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
                    println!(
                        "{} {}  model={}  msgs={}  updated={}",
                        if active {
                            "▶".green().to_string()
                        } else {
                            "  ".to_string()
                        },
                        id.dimmed(),
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
        let outcome = outcome?;
        self.session = Some(session);
        if let Some(session) = &self.session {
            self.store.save(session)?;
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
}

fn print_help() {
    println!("{}", "── 命令 ──".bold());
    println!("  直接输入文字        向当前 Agent 发起任务");
    println!("  /new [模型]         新建会话");
    println!("  /sessions           列出会话");
    println!("  /resume <id>        恢复会话");
    println!("  /model [名称]       查看/切换模型");
    println!("  /plan | /build      切换只读规划模式 / 执行模式");
    println!("  /diff               查看本次会话文件改动");
    println!("  /undo               回滚本次会话全部写操作");
    println!("  /status             查看工作区/模型/会话状态");
    println!("  /permissions        查看权限策略");
    println!("  /audit              查看最近审计记录");
    println!("  /mcp add|list|remove  管理 MCP 服务器");
    println!("  /skills             列出已加载技能");
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
