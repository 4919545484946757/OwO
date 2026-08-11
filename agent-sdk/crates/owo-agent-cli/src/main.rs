use async_trait::async_trait;
use clap::{Parser, Subcommand};
use owo_agent_core::permissions::{Approver, AutoApprover, Decision, PermissionRequest, Policy};
use owo_agent_core::session::Session;
use owo_agent_core::tools::ToolRegistry;
use owo_agent_core::{
    Agent, AgentConfig, JsonSessionStore, OpenAiCompatibleConfig, OpenAiCompatibleProvider,
    TurnEvent,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "owo-agent", version, about = "OwO Agent SDK CLI（Codex 式）")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 在指定工作区执行一轮 Agent 任务
    Turn(TurnArgs),
    /// 启动本地 HTTP API 服务
    Serve(ServeArgs),
}

#[derive(clap::Args)]
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

#[derive(clap::Args)]
struct ServeArgs {
    #[arg(long, default_value_t = 4096)]
    port: u16,
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Commands::Turn(args) => run_turn(args).await?,
        Commands::Serve(args) => run_serve(args).await?,
    }
    Ok(())
}

fn build_agent(workspace: &std::path::Path) -> Result<Agent, Box<dyn std::error::Error>> {
    let config = OpenAiCompatibleConfig::from_env()?;
    let provider = Arc::new(OpenAiCompatibleProvider::new(config)?);
    let policy = Policy::new(workspace.to_path_buf());
    let registry = ToolRegistry::new();
    Ok(Agent::new(
        provider,
        registry,
        policy,
        AgentConfig::default(),
    ))
}

async fn run_turn(args: TurnArgs) -> Result<(), Box<dyn std::error::Error>> {
    let workspace = args.workspace.canonicalize()?;
    let model = args.model.unwrap_or_else(|| {
        std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-5.1-codex".to_string())
    });
    let agent = build_agent(&workspace)?;
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
        Arc::new(ConsoleApprover)
    };

    let mut on_event = |event: &TurnEvent| match event {
        TurnEvent::ModelCall => println!("[model] 调用模型…"),
        TurnEvent::PermissionRequest(request) => println!(
            "[审批] 需要 {} 权限：{}（{}）",
            request.level.label(),
            request.tool,
            request.reason
        ),
        TurnEvent::ToolStart { tool, .. } => println!("[工具] {tool} …"),
        TurnEvent::ToolResult {
            tool, ok, error, ..
        } => {
            if *ok {
                println!("[工具] {tool} 完成");
            } else {
                println!(
                    "[工具] {tool} 失败：{}",
                    error.as_deref().unwrap_or("未知错误")
                );
            }
        }
        TurnEvent::Final { text } => println!("\n=== 最终结果 ===\n{text}"),
    };

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
    let agent = build_agent(&workspace)?;
    let data_root = std::env::var("OWO_AGENT_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("LOCALAPPDATA")
                .map(|dir| PathBuf::from(dir).join("OwO").join("Agent"))
                .unwrap_or_else(|_| PathBuf::from("data/agent"))
        });
    let store = JsonSessionStore::new(data_root.join("sessions"));
    let state = Arc::new(owo_agent_server::AppState::new(agent, store));
    let app = owo_agent_server::build_router(state);
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], args.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("owo-agent server listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

struct ConsoleApprover;

#[async_trait]
impl Approver for ConsoleApprover {
    async fn decide(&self, request: &PermissionRequest) -> Decision {
        use std::io::Write;
        use tokio::io::AsyncBufReadExt;
        print!(
            "  允许 {} 执行 {} ? [y/N] ",
            request.level.label(),
            request.tool
        );
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
        if reader.read_line(&mut line).await.is_ok() {
            match line.trim().to_lowercase().as_str() {
                "y" | "yes" => return Decision::Allow,
                _ => return Decision::Deny,
            }
        }
        Decision::Deny
    }
}
