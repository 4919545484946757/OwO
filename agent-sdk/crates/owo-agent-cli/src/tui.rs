//! OpenCode 式全屏 TUI（ratatui + crossterm）。

use crate::{
    build_agent_with_mcp, connect_mcp_clients, display_path, ensure_data_root, load_mcp_configs,
    resolve_model, save_mcp_configs, AGENTS_TEMPLATE,
};
use async_trait::async_trait;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use owo_agent_core::permissions::{Approver, Decision, PermissionRequest};
use owo_agent_core::session::{Session, SessionStore};
use owo_agent_core::{
    export_html, export_markdown, Agent, JsonSessionStore, McpClient, McpServerConfig,
    SkillRegistry, TurnEvent,
};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(clap::Args)]
pub struct TuiArgs {
    #[arg(long, default_value = ".")]
    pub workspace: PathBuf,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long, default_value = "build")]
    pub agent: String,
    /// 自动允许所有审批（仅测试用）
    #[arg(long)]
    pub no_approval: bool,
    /// 覆盖数据目录
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
}

pub fn run(args: TuiArgs) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let workspace = args.workspace.canonicalize()?;
    let model = resolve_model(args.model);
    let read_only = args.agent == "plan";
    let root = ensure_data_root(args.data_dir, &workspace);
    let store = JsonSessionStore::new(root.join("sessions"));
    let mcp_configs = load_mcp_configs(&root);
    let mcp_clients = runtime.block_on(connect_mcp_clients(&mcp_configs));
    let skills = SkillRegistry::discover(&workspace, &root);
    let agent = Arc::new(build_agent_with_mcp(
        &workspace,
        &model,
        read_only,
        &mcp_clients,
        &skills,
    )?);
    let mut app = TuiApp::new(
        workspace,
        model,
        read_only,
        args.no_approval,
        root,
        store,
        agent,
        mcp_configs,
        mcp_clients,
        skills,
    );
    let terminal = ratatui::init();
    let result = app.run(&runtime, terminal);
    ratatui::restore();
    result
}

type PendingApprovals = Arc<Mutex<HashMap<String, Sender<Decision>>>>;

struct TuiApprover {
    pending: PendingApprovals,
}

#[async_trait]
impl Approver for TuiApprover {
    async fn decide(&self, request: &PermissionRequest) -> Decision {
        let (tx, rx) = mpsc::channel();
        if let Ok(mut map) = self.pending.lock() {
            map.insert(request.request_id.clone(), tx);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(300);
        loop {
            if let Ok(decision) = rx.try_recv() {
                return decision;
            }
            if std::time::Instant::now() >= deadline {
                if let Ok(mut map) = self.pending.lock() {
                    map.remove(&request.request_id);
                }
                return Decision::Deny;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

enum TuiMsg {
    Event(TurnEvent),
    Finished(Result<(usize, Option<String>), String>, Box<Session>),
}

struct TuiApp {
    workspace: PathBuf,
    model: String,
    read_only: bool,
    no_approval: bool,
    data_root: PathBuf,
    store: JsonSessionStore,
    session: Option<Session>,
    agent: Arc<Agent>,
    abort: Arc<AtomicBool>,
    pending: PendingApprovals,
    pending_order: Vec<String>,
    event_rx: Option<Receiver<TuiMsg>>,
    input: String,
    transcript: Vec<(String, Style)>,
    streaming: String,
    scroll: usize,
    running: bool,
    status: String,
    mcp_configs: Vec<McpServerConfig>,
    mcp_clients: Vec<(String, Arc<tokio::sync::Mutex<McpClient>>)>,
    skills: SkillRegistry,
}

impl TuiApp {
    #[allow(clippy::too_many_arguments)]
    fn new(
        workspace: PathBuf,
        model: String,
        read_only: bool,
        no_approval: bool,
        data_root: PathBuf,
        store: JsonSessionStore,
        agent: Arc<Agent>,
        mcp_configs: Vec<McpServerConfig>,
        mcp_clients: Vec<(String, Arc<tokio::sync::Mutex<McpClient>>)>,
        skills: SkillRegistry,
    ) -> Self {
        Self {
            workspace,
            model,
            read_only,
            no_approval,
            data_root,
            store,
            session: None,
            agent,
            abort: Arc::new(AtomicBool::new(false)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            pending_order: Vec::new(),
            event_rx: None,
            input: String::new(),
            transcript: vec![(
                "OwO Agent TUI — 输入文字开始任务，Tab 切换 build/plan，Ctrl+C 中止/退出，/help 查看命令"
                    .to_string(),
                dim(),
            )],
            streaming: String::new(),
            scroll: 0,
            running: false,
            status: "就绪".to_string(),
            mcp_configs,
            mcp_clients,
            skills,
        }
    }

    fn run(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        mut terminal: Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if self.handle_key(key, runtime)? {
                        break;
                    }
                }
            }
            self.drain_events();
            if self.running && self.pending_order.is_empty() {
                self.status = "回合进行中（Ctrl+C 中止）".to_string();
            }
        }
        Ok(())
    }

    fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(8),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(area);

        let title = Line::from(vec![
            Span::styled(
                " OwO Agent ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                display_path(&self.workspace),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(" | "),
            Span::styled(&self.model, Style::default().fg(Color::Blue)),
            Span::raw(" | "),
            Span::styled(
                if self.read_only { "plan" } else { "build" },
                Style::default().fg(if self.read_only {
                    Color::Yellow
                } else {
                    Color::Green
                }),
            ),
            Span::raw(" | "),
            Span::styled(
                if self.running {
                    "● 运行中"
                } else {
                    "○ 空闲"
                },
                Style::default().fg(if self.running {
                    Color::Red
                } else {
                    Color::Green
                }),
            ),
        ]);
        frame.render_widget(Paragraph::new(title), chunks[0]);

        let mut visible = self
            .visible_lines(chunks[1].height as usize)
            .into_iter()
            .collect::<Vec<_>>();
        if !self.streaming.is_empty() {
            visible.push((
                format!("▍{}", self.streaming),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            ));
        }
        let lines: Vec<Line> = visible
            .iter()
            .map(|(text, style)| Line::from(Span::styled(text.clone(), *style)))
            .collect();
        let transcript = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" 会话 "))
            .wrap(Wrap { trim: false });
        frame.render_widget(transcript, chunks[1]);

        let mode_hint = if self.read_only {
            "plan（只读）"
        } else {
            "build"
        };
        let input_block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" 输入（{mode_hint}） "));
        frame.render_widget(
            Paragraph::new(self.input.as_str())
                .block(input_block)
                .wrap(Wrap { trim: false }),
            chunks[2],
        );

        let status_line = Line::from(vec![
            Span::styled(" Tab ", Style::default().fg(Color::Cyan)),
            Span::raw("模式 "),
            Span::styled(" Ctrl+C ", Style::default().fg(Color::Cyan)),
            Span::raw("中止/退出 "),
            Span::styled(" PgUp/PgDn ", Style::default().fg(Color::Cyan)),
            Span::raw("滚动 | "),
            Span::styled(&self.status, Style::default().fg(Color::Yellow)),
        ]);
        frame.render_widget(Paragraph::new(status_line), chunks[3]);
    }

    fn visible_lines(&self, height: usize) -> Vec<(String, Style)> {
        let viewport = height.saturating_sub(2).max(1);
        let end = self.transcript.len().saturating_sub(self.scroll);
        let start = end.saturating_sub(viewport);
        self.transcript[start..end].to_vec()
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        runtime: &tokio::runtime::Runtime,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        if self.running {
            match key.code {
                KeyCode::Char('y' | 'Y') => self.respond_approval(true),
                KeyCode::Char('n' | 'N') => self.respond_approval(false),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.abort.store(true, Ordering::Relaxed);
                    self.push_system("正在中止当前回合…".to_string(), yellow());
                }
                _ => {}
            }
            return Ok(false);
        }

        match key.code {
            KeyCode::Enter => self.submit(runtime)?,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Ok(true);
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.transcript.clear();
            }
            KeyCode::Char(c) => self.input.push(c),
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Tab => self.toggle_mode()?,
            KeyCode::Esc => self.input.clear(),
            KeyCode::PageUp => self.scroll += 8,
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(8),
            _ => {}
        }
        Ok(false)
    }

    fn submit(
        &mut self,
        runtime: &tokio::runtime::Runtime,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let line = std::mem::take(&mut self.input);
        let line = line.trim().to_string();
        if line.is_empty() {
            return Ok(());
        }
        if let Some(command) = line.strip_prefix('/') {
            self.handle_command(command, runtime)?;
            return Ok(());
        }
        self.start_turn(runtime, &line);
        Ok(())
    }

    fn start_turn(&mut self, runtime: &tokio::runtime::Runtime, prompt: &str) {
        if self.session.is_none() {
            match self.store.create(&self.workspace, &self.model, None) {
                Ok(session) => {
                    let id = session.id.clone();
                    self.session = Some(session);
                    self.push_system(format!("新会话：{id}"), green());
                }
                Err(error) => {
                    self.push_system(format!("创建会话失败：{error}"), red());
                    return;
                }
            }
        }
        let mut session = self.session.take().expect("session created");
        self.abort.store(false, Ordering::Relaxed);
        self.pending_order.clear();
        self.scroll = 0;
        self.running = true;
        self.status = "调用模型…".to_string();
        self.streaming.clear();
        self.push_line(format!("▶ {prompt}"), cyan());

        let agent = Arc::clone(&self.agent);
        let abort = Arc::clone(&self.abort);
        let pending = Arc::clone(&self.pending);
        let (tx, rx) = mpsc::channel::<TuiMsg>();
        self.event_rx = Some(rx);
        let no_approval = self.no_approval;
        let prompt_owned = prompt.to_string();
        runtime.spawn(async move {
            let approver = if no_approval {
                Box::new(owo_agent_core::permissions::AutoApprover { allow: true })
                    as Box<dyn Approver>
            } else {
                Box::new(TuiApprover { pending }) as Box<dyn Approver>
            };
            let mut on_event = |event: &TurnEvent| {
                let _ = tx.send(TuiMsg::Event(event.clone()));
            };
            let outcome = agent
                .run_turn(
                    &mut session,
                    &prompt_owned,
                    approver.as_ref(),
                    &abort,
                    &mut on_event,
                )
                .await;
            let result = outcome
                .map(|outcome| (outcome.steps, outcome.final_text))
                .map_err(|error| error.to_string());
            let _ = tx.send(TuiMsg::Finished(result, Box::new(session)));
        });
    }

    fn drain_events(&mut self) {
        loop {
            let message = self.event_rx.as_ref().and_then(|rx| rx.try_recv().ok());
            match message {
                Some(TuiMsg::Event(event)) => self.push_event(event),
                Some(TuiMsg::Finished(result, session)) => {
                    self.running = false;
                    self.event_rx = None;
                    self.session = Some(*session);
                    if let Some(session) = &self.session {
                        let _ = self.store.save(session);
                    }
                    match result {
                        Ok((steps, final_text)) => {
                            let changed = self
                                .session
                                .as_ref()
                                .map(|session| session.diff().len())
                                .unwrap_or(0);
                            self.push_system(
                                format!(
                                    "✓ 完成：工具 {steps} 步，改动 {changed} 个文件（/diff 查看，/undo 回滚）"
                                ),
                                green(),
                            );
                            if let Some(text) = final_text {
                                self.push_line("── 结果 ──".to_string(), bold());
                                self.push_line(text, default());
                            }
                            self.status = "就绪".to_string();
                        }
                        Err(error) => {
                            self.push_system(format!("回合失败：{error}"), red());
                            self.status = "出错".to_string();
                        }
                    }
                }
                None => break,
            }
        }
    }

    fn push_event(&mut self, event: TurnEvent) {
        match event {
            TurnEvent::ModelCall => {
                self.flush_streaming();
                self.push_system("↻ 调用模型…".to_string(), dim());
            }
            TurnEvent::TokenDelta { delta } => {
                self.streaming.push_str(&delta);
            }
            TurnEvent::Compaction { summary } => {
                self.flush_streaming();
                self.push_system(format!("✦ 上下文已压缩：{summary}"), yellow());
            }
            TurnEvent::PermissionRequest(request) => {
                self.flush_streaming();
                self.pending_order.push(request.request_id.clone());
                self.push_line(
                    format!(
                        "审批：需要 {} 权限执行 {}（{}）— 按 y 允许 / n 拒绝",
                        request.level.label(),
                        request.tool,
                        request.reason
                    ),
                    yellow(),
                );
                self.status = "等待审批（y/n）".to_string();
            }
            TurnEvent::ToolStart { tool, .. } => {
                self.flush_streaming();
                self.push_line(format!("▶ {tool} …"), blue());
            }
            TurnEvent::ToolResult {
                tool, ok, error, ..
            } => {
                self.flush_streaming();
                if ok {
                    self.push_line(format!("✔ {tool}"), green());
                } else {
                    self.push_line(
                        format!("✘ {tool}：{}", error.unwrap_or_else(|| "未知错误".into())),
                        red(),
                    );
                }
            }
            TurnEvent::Final { text } => {
                if self.streaming.is_empty() {
                    self.push_line("── 结果 ──".to_string(), bold());
                    self.push_line(text, default());
                } else {
                    self.flush_streaming();
                }
            }
        }
    }

    fn flush_streaming(&mut self) {
        if !self.streaming.is_empty() {
            let text = std::mem::take(&mut self.streaming);
            self.push_line(text, default());
        }
    }

    fn respond_approval(&mut self, allow: bool) {
        let Some(request_id) = self.pending_order.pop() else {
            return;
        };
        let sent = self
            .pending
            .lock()
            .ok()
            .and_then(|mut map| map.remove(&request_id))
            .map(|tx| {
                tx.send(if allow {
                    Decision::Allow
                } else {
                    Decision::Deny
                })
                .is_ok()
            })
            .unwrap_or(false);
        if sent {
            self.push_system(
                if allow {
                    "→ 已允许".to_string()
                } else {
                    "→ 已拒绝".to_string()
                },
                if allow { green() } else { red() },
            );
            self.status = "执行中…".to_string();
        }
    }

    fn toggle_mode(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.read_only = !self.read_only;
        self.rebuild_agent()?;
        self.push_system(
            if self.read_only {
                "已切换 plan（只读）".to_string()
            } else {
                "已切换 build".to_string()
            },
            yellow(),
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

    fn handle_command(
        &mut self,
        command: &str,
        runtime: &tokio::runtime::Runtime,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut parts = command.split_whitespace();
        match parts.next().unwrap_or_default() {
            "help" => self.push_help(),
            "exit" | "quit" => std::process::exit(0),
            "new" => {
                if let Some(session) = &self.session {
                    let _ = self.store.save(session);
                }
                let session = self.store.create(&self.workspace, &self.model, None)?;
                let id = session.id.clone();
                self.session = Some(session);
                self.push_system(format!("新会话：{id}"), green());
            }
            "sessions" => {
                let ids = self.store.list();
                if ids.is_empty() {
                    self.push_system("暂无会话（/new 创建）".to_string(), dim());
                } else {
                    for id in ids {
                        if let Ok(session) = self.store.load(&id) {
                            let active = self
                                .session
                                .as_ref()
                                .map(|current| current.id == id)
                                .unwrap_or(false);
                            self.push_line(
                                format!(
                                    "{}{}  model={}  msgs={}",
                                    if active { "▶ " } else { "  " },
                                    id,
                                    session.model,
                                    session.messages.len()
                                ),
                                if active { green() } else { default() },
                            );
                        }
                    }
                }
            }
            "resume" => {
                let id = parts.next().ok_or("用法：/resume <会话ID>")?;
                let session = self.store.load(id)?;
                self.session = Some(session);
                self.push_system(format!("已恢复会话：{id}"), green());
            }
            "model" => match parts.next() {
                Some(model) => {
                    self.model = model.to_string();
                    self.rebuild_agent()?;
                    self.push_system(format!("模型已切换：{model}"), green());
                }
                None => self.push_system(format!("当前模型：{}", self.model), dim()),
            },
            "diff" => self.show_diff(),
            "undo" | "revert" => {
                let Some(session) = &mut self.session else {
                    self.push_system("暂无会话".to_string(), dim());
                    return Ok(());
                };
                let restored = runtime.block_on(session.revert())?;
                let _ = self.store.save(session);
                if restored.is_empty() {
                    self.push_system("没有可回滚的改动".to_string(), dim());
                } else {
                    self.push_system(format!("已回滚：{}", restored.join(", ")), green());
                }
            }
            "mcp" => self.handle_mcp(command, runtime)?,
            "skills" => self.list_skills(),
            "fork" => self.fork_session(parts.next())?,
            "rewind" => {
                let keep = parts.next().ok_or("用法：/rewind <保留消息数>")?;
                self.rewind_session(keep)?;
            }
            "redo" => self.redo_session()?,
            "tree" => self.show_tree(),
            "share" => self.share_session(parts.next())?,
            "plan" => {
                if !self.read_only {
                    self.toggle_mode()?;
                }
            }
            "build" => {
                if self.read_only {
                    self.toggle_mode()?;
                }
            }
            "status" => self.push_status(),
            "init" => {
                let target = self.workspace.join("AGENTS.md");
                if target.exists() {
                    self.push_system(
                        format!("AGENTS.md 已存在：{}", display_path(&target)),
                        yellow(),
                    );
                } else {
                    std::fs::write(&target, AGENTS_TEMPLATE)?;
                    self.push_system(format!("已生成 {}", display_path(&target)), green());
                }
            }
            "clear" => self.transcript.clear(),
            other => self.push_system(format!("未知命令：/{other}（/help 查看）"), red()),
        }
        Ok(())
    }

    fn list_skills(&mut self) {
        let skills: Vec<(String, String)> = self
            .agent
            .skills()
            .list()
            .iter()
            .map(|skill| (skill.name.clone(), skill.description.clone()))
            .collect();
        if skills.is_empty() {
            self.push_system(
                format!(
                    "暂无技能（放置到 {}/skills 或 .agents/skills/，每技能一个含 SKILL.md 的目录）",
                    display_path(&self.data_root)
                ),
                dim(),
            );
            return;
        }
        for (name, description) in skills {
            self.push_line(format!("{name}：{description}"), default());
        }
    }

    fn fork_session(&mut self, index: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let Some(current) = &self.session else {
            self.push_system("暂无会话".to_string(), dim());
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
        self.push_system(
            format!("已创建子会话 {id}（消息 {index} 处 fork）"),
            green(),
        );
        Ok(())
    }

    fn rewind_session(&mut self, keep: &str) -> Result<(), Box<dyn std::error::Error>> {
        let Some(session) = &mut self.session else {
            self.push_system("暂无会话".to_string(), dim());
            return Ok(());
        };
        let keep: usize = keep.parse().map_err(|_| "保留消息数需为数字".to_string())?;
        let removed = session.rewind(keep);
        self.store.save(session)?;
        self.push_system(
            format!(
                "已回退到 {keep} 条消息（移除 {} 条，/redo 可恢复）",
                removed.len()
            ),
            yellow(),
        );
        Ok(())
    }

    fn redo_session(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let Some(session) = &mut self.session else {
            self.push_system("暂无会话".to_string(), dim());
            return Ok(());
        };
        let restored = session.redo().map(|tail| tail.len()).unwrap_or(0);
        self.store.save(session)?;
        if restored == 0 {
            self.push_system("没有可恢复的历史".to_string(), dim());
        } else {
            self.push_system(format!("已恢复 {restored} 条消息"), green());
        }
        Ok(())
    }

    fn show_tree(&mut self) {
        let mut lines = Vec::new();
        for id in self.store.list() {
            if let Ok(session) = self.store.load(&id) {
                let active = self
                    .session
                    .as_ref()
                    .map(|current| current.id == id)
                    .unwrap_or(false);
                let parent = session
                    .parent_id
                    .clone()
                    .unwrap_or_else(|| "(根)".to_string());
                let fork = session
                    .fork_point
                    .map(|point| point.to_string())
                    .unwrap_or_else(|| "-".to_string());
                lines.push(format!(
                    "{}{} parent={} fork@{} msgs={}",
                    if active { "▶ " } else { "  " },
                    id,
                    parent,
                    fork,
                    session.messages.len()
                ));
            }
        }
        if lines.is_empty() {
            self.push_system("暂无会话".to_string(), dim());
        } else {
            for line in lines {
                self.push_line(line, default());
            }
        }
    }

    fn share_session(&mut self, format: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let Some(session) = &self.session else {
            self.push_system("暂无会话".to_string(), dim());
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
        self.push_system(format!("已导出会话分享：{}", display_path(&path)), green());
        Ok(())
    }

    fn handle_mcp(
        &mut self,
        command: &str,
        runtime: &tokio::runtime::Runtime,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let rest = command.trim_start_matches("mcp").trim_start().to_string();
        let mut parts = rest.split_whitespace();
        match parts.next() {
            Some("add") => {
                let name = parts
                    .next()
                    .ok_or("用法：/mcp add <名称> <命令> [参数...]")?
                    .to_string();
                let command_line: Vec<&str> = parts.collect();
                let command = command_line.first().ok_or("缺少 MCP 服务器命令")?;
                let config = McpServerConfig {
                    name: name.clone(),
                    command: command.to_string(),
                    args: command_line[1..]
                        .iter()
                        .map(|arg| arg.to_string())
                        .collect(),
                };
                match runtime.block_on(McpClient::connect(&config)) {
                    Ok(client) => {
                        let tool_count = client.tools().len();
                        self.mcp_clients
                            .push((name.clone(), Arc::new(tokio::sync::Mutex::new(client))));
                        self.mcp_configs.push(config);
                        save_mcp_configs(&self.data_root, &self.mcp_configs);
                        self.rebuild_agent()?;
                        self.push_system(
                            format!("已添加 MCP {name}（{tool_count} 个工具）"),
                            green(),
                        );
                    }
                    Err(error) => self.push_system(format!("MCP {name} 连接失败：{error}"), red()),
                }
            }
            Some("list") => {
                if self.mcp_clients.is_empty() {
                    self.push_system(
                        "未配置 MCP 服务器（/mcp add <名称> <命令>）".to_string(),
                        dim(),
                    );
                } else {
                    let mut lines = Vec::new();
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
                        lines.push(format!("{name}：{tool_names}"));
                    }
                    for line in lines {
                        self.push_line(line, default());
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
                    let _ = runtime.block_on(async { client.lock().await.shutdown().await });
                }
                self.mcp_configs.retain(|config| config.name != name);
                save_mcp_configs(&self.data_root, &self.mcp_configs);
                self.rebuild_agent()?;
                self.push_system(format!("已移除 MCP 服务器：{name}"), green());
            }
            _ => self.push_system(
                "用法：/mcp add <名称> <命令> [参数...] | /mcp list | /mcp remove <名称>"
                    .to_string(),
                dim(),
            ),
        }
        Ok(())
    }

    fn show_diff(&mut self) {
        let mut lines = vec![("当前会话没有未回滚的改动".to_string(), dim())];
        if let Some(session) = &self.session {
            let diffs = session.diff();
            if !diffs.is_empty() {
                lines.clear();
                for diff in diffs {
                    lines.push((format!("● {}", diff.path), cyan()));
                    if let Some(before) = diff.before {
                        for line in before.lines() {
                            lines.push((format!("- {line}"), red()));
                        }
                    } else {
                        lines.push(("(新建文件)".to_string(), green()));
                    }
                    if let Some(after) = diff.after {
                        for line in after.lines() {
                            lines.push((format!("+ {line}"), green()));
                        }
                    } else {
                        lines.push(("(已删除)".to_string(), red()));
                    }
                }
            }
        }
        self.transcript.extend(lines);
    }

    fn push_status(&mut self) {
        let session_info = self.session.as_ref().map(|session| {
            (
                session.id.clone(),
                session.messages.len(),
                session.diff().len(),
            )
        });
        self.push_line(
            format!("工作区：{}", display_path(&self.workspace)),
            default(),
        );
        self.push_line(format!("模型：{}", self.model), default());
        self.push_line(
            format!(
                "模式：{}",
                if self.read_only {
                    "plan（只读）"
                } else {
                    "build"
                }
            ),
            default(),
        );
        match session_info {
            Some((id, messages, diffs)) => {
                self.push_line(format!("会话：{id}"), default());
                self.push_line(format!("消息数：{messages}"), default());
                self.push_line(format!("未回滚改动：{diffs}"), default());
            }
            None => self.push_line("会话：无（任务时自动创建）".to_string(), dim()),
        }
        let audit_count = match self.agent.audit_log().lock() {
            Ok(guard) => guard.entries.len(),
            Err(_) => 0,
        };
        self.push_line(format!("审计记录：{audit_count} 条"), default());
    }

    fn push_help(&mut self) {
        for line in [
            "直接输入文字 发起任务",
            "/new /sessions /resume <id>  会话管理",
            "/fork [序号] /rewind <条数> /redo /tree  会话分支/回退/恢复/树",
            "/share [html]  导出会话分享",
            "/model [名称]  查看/切换模型",
            "/plan /build  切换只读/执行模式（或 Tab）",
            "/diff /undo  查看改动 / 回滚",
            "/mcp add/list/remove  MCP 服务器",
            "/skills  列出已加载技能",
            "/status /init /clear",
            "/exit 退出（或 Ctrl+C）",
        ] {
            self.push_line(format!("  {line}"), dim());
        }
    }

    fn push_line(&mut self, text: String, style: Style) {
        self.transcript.push((text, style));
    }

    fn push_system(&mut self, text: String, style: Style) {
        self.push_line(text, style);
    }
}

fn default() -> Style {
    Style::default()
}
fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}
fn cyan() -> Style {
    Style::default().fg(Color::Cyan)
}
fn blue() -> Style {
    Style::default().fg(Color::Blue)
}
fn green() -> Style {
    Style::default().fg(Color::Green)
}
fn yellow() -> Style {
    Style::default().fg(Color::Yellow)
}
fn red() -> Style {
    Style::default().fg(Color::Red)
}
fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> TuiApp {
        std::env::set_var("OPENAI_API_KEY", "test");
        std::env::set_var("OPENAI_BASE_URL", "http://127.0.0.1:9");
        std::env::set_var("OPENAI_MODEL", "mock");
        let workspace = std::env::temp_dir();
        let store = JsonSessionStore::new(workspace.join("owo-tui-test-sessions"));
        let agent = Arc::new(crate::build_agent(&workspace, "mock", false).unwrap());
        TuiApp::new(
            workspace,
            "mock".to_string(),
            false,
            true,
            std::env::temp_dir().join("owo-tui-test-root"),
            store,
            agent,
            Vec::new(),
            Vec::new(),
            SkillRegistry::default(),
        )
    }

    #[test]
    fn permission_request_queues_approval_and_responds() {
        let mut app = test_app();
        let request = PermissionRequest {
            request_id: "req-1".to_string(),
            tool: "write_file".to_string(),
            args: serde_json::json!({ "path": "a.txt" }),
            level: owo_agent_core::permissions::Level::Write,
            reason: "测试".to_string(),
        };
        let (tx, rx) = mpsc::channel();
        app.pending
            .lock()
            .unwrap()
            .insert(request.request_id.clone(), tx);
        app.pending_order.push(request.request_id.clone());

        app.respond_approval(true);

        assert_eq!(rx.try_recv().unwrap(), Decision::Allow);
        assert!(app.pending_order.is_empty());
    }

    #[test]
    fn transcript_visible_lines_respects_scroll() {
        let mut app = test_app();
        for index in 0..20 {
            app.push_line(format!("line {index}"), default());
        }
        app.scroll = 0;
        assert_eq!(app.visible_lines(5).last().unwrap().0, "line 19");
        app.scroll = 5;
        assert_eq!(app.visible_lines(5).last().unwrap().0, "line 14");
    }
}
