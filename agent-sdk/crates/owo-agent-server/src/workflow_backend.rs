//! 工作流真实后端（Agent 1，R5 子任务 1）。
//!
//! 独立编译模块（不使用 crate::/super::；server 类型全限定 owo_agent_server::AppState）。
//! 提供：
//! - `WfEvents`：per-run 广播 + 历史重放（run 级 SSE 事件源，禁止改 sse.rs）。
//! - `ChannelApprover`：真实人审——注册 pending oneshot，面板经
//!   `POST /workflow/run/{run_id}/approval` 裁决；超时（默认 120s）→ Rejected。
//! - `EventBackend<B>`：包装任意 ActionBackend，把动作调用推为事件帧。
//! - `ServerActionBackend`：真实后端——sense（clipboard/foreground/workspace）、
//!   locate（scene 图定位）、act（写文件校验 workspace 边界；桌面动作默认门禁拒绝）、
//!   invoke_skill（技能包入口检查）、invoke_mcp（McpRegistry）、notify（审计）。
//!
//! 接线：workflow_api.rs 以 `#[path = "workflow_backend.rs"] mod workflow_backend;` 自包含编译。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use owo_agent_core::workflow::{
    ActSpec, ActionBackend, Approval, HumanApprover, LocateSpec, SenseSpec,
};

// ----------------------------------------------------------------------------
// 事件流（per-run 广播 + 历史重放）
// ----------------------------------------------------------------------------

/// SSE 事件帧：`event: <name>\ndata: <json>\n\n`。
pub fn event_frame(name: &str, data: &serde_json::Value) -> String {
    format!("event: {name}\ndata: {}\n\n", data)
}

/// per-run 事件源：broadcast + 历史（订阅先重放历史再流式）。
pub struct WfEvents {
    tx: tokio::sync::broadcast::Sender<String>,
    history: Mutex<Vec<String>>,
}

impl WfEvents {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(capacity);
        Self {
            tx,
            history: Mutex::new(Vec::new()),
        }
    }

    /// 推送一帧（历史 + 广播；无订阅者不阻塞）。
    pub fn push(&self, name: &str, data: &serde_json::Value) {
        let frame = event_frame(name, data);
        self.history.lock().unwrap().push(frame.clone());
        let _ = self.tx.send(frame);
    }

    /// 事件帧（含步骤/动作名）便捷推送。
    pub fn action(&self, kind: &str, run_id: &str, detail: &str) {
        self.push(
            kind,
            &serde_json::json!({
                "run_id": run_id,
                "detail": detail,
                "ts": chrono::Utc::now().to_rfc3339(),
            }),
        );
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.tx.subscribe()
    }

    pub fn history(&self) -> Vec<String> {
        self.history.lock().unwrap().clone()
    }
}

// ----------------------------------------------------------------------------
// 真实人审（oneshot 通道 + 超时）
// ----------------------------------------------------------------------------

/// 待审批记录（面板展示）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PendingApprovalInfo {
    pub id: String,
    pub run_id: String,
    pub prompt: String,
    pub created_at: String,
}

/// 全局 pending 注册表：id → (run_id, prompt, created_at, 裁决通道)。
static PENDING: OnceLock<Arc<Mutex<HashMapLock>>> = OnceLock::new();

type HashMapLock =
    std::collections::HashMap<String, (String, String, String, tokio::sync::oneshot::Sender<bool>)>;

fn pending_map() -> &'static Arc<Mutex<HashMapLock>> {
    PENDING.get_or_init(|| Arc::new(Mutex::new(std::collections::HashMap::new())))
}

/// 面板裁决入口：POST /workflow/run/{run_id}/approval。
/// 返回 Ok(true) 表示已裁决；Err 为可读错误（未知 run / 无 pending）。
pub fn decide_approval(run_id: &str, approve: bool) -> Result<(), String> {
    let mut map = pending_map().lock().unwrap();
    let keys: Vec<String> = map
        .iter()
        .filter(|(_, (r, _, _, _))| r == run_id)
        .map(|(id, _)| id.clone())
        .collect();
    let key = keys
        .first()
        .ok_or_else(|| format!("运行 {run_id} 没有等待审批的请求"))?;
    let (_, _, _, tx) = map.remove(key).expect("key 存在");
    let _ = tx.send(approve);
    Ok(())
}

/// 真实人审实现：request() 注册 pending 并等待裁决；超时 → Rejected。
pub struct ChannelApprover {
    pub run_id: String,
    pub timeout: Duration,
    /// 事件推送（approval_required / 超时审计）。
    pub events: Option<Arc<WfEvents>>,
    /// 步骤序号（事件展示）。
    pub step_seq: Mutex<u32>,
}

impl ChannelApprover {
    pub fn new(run_id: impl Into<String>, timeout: Duration) -> Self {
        Self {
            run_id: run_id.into(),
            timeout,
            events: None,
            step_seq: Mutex::new(0),
        }
    }

    pub fn with_events(mut self, events: Arc<WfEvents>) -> Self {
        self.events = Some(events);
        self
    }
}

#[async_trait]
impl HumanApprover for ChannelApprover {
    async fn request(&self, prompt: &str) -> Approval {
        let id = format!("wf-{}-{}", self.run_id, uuid::Uuid::new_v4().simple());
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut map = pending_map().lock().unwrap();
            map.insert(
                id.clone(),
                (
                    self.run_id.clone(),
                    prompt.to_string(),
                    chrono::Utc::now().to_rfc3339(),
                    tx,
                ),
            );
        }
        let info = PendingApprovalInfo {
            id: id.clone(),
            run_id: self.run_id.clone(),
            prompt: prompt.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        if let Some(events) = &self.events {
            let seq = {
                let mut s = self.step_seq.lock().unwrap();
                *s += 1;
                *s
            };
            events.push(
                "approval_required",
                &serde_json::json!({
                    "run_id": self.run_id,
                    "approval": info,
                    "step_seq": seq,
                }),
            );
        }
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(true)) => Approval::Approved,
            Ok(Ok(false)) => {
                {
                    let mut map = pending_map().lock().unwrap();
                    map.remove(&id);
                }
                if let Some(events) = &self.events {
                    events.action("approval_rejected", &self.run_id, &format!("{id} 被拒绝"));
                }
                Approval::Rejected
            }
            Ok(Err(_)) | Err(_) => {
                // 超时/通道关闭：必须清理 pending 记录，否则快照恒为 waiting_approval。
                {
                    let mut map = pending_map().lock().unwrap();
                    map.remove(&id);
                }
                if let Some(events) = &self.events {
                    events.action(
                        "approval_timed_out",
                        &self.run_id,
                        &format!("{id} 审批超时（{}s）", self.timeout.as_secs()),
                    );
                }
                Approval::Rejected
            }
        }
    }
}

/// 当前 pending 审批清单（面板轮询用）。
pub fn pending_approvals(run_id: &str) -> Vec<PendingApprovalInfo> {
    let mut out = Vec::new();
    let map = pending_map().lock().unwrap();
    for (id, (run, prompt, created_at, _)) in map.iter() {
        if run == run_id {
            out.push(PendingApprovalInfo {
                id: id.clone(),
                run_id: run.clone(),
                prompt: prompt.clone(),
                created_at: created_at.clone(),
            });
        }
    }
    out
}

// ----------------------------------------------------------------------------
// 事件包装后端
// ----------------------------------------------------------------------------

/// 包装任意 ActionBackend：每次动作调用前后推送事件帧。
pub struct EventBackend<B> {
    pub inner: B,
    pub events: Arc<WfEvents>,
    pub run_id: String,
}

impl<B> EventBackend<B> {
    pub fn new(inner: B, events: Arc<WfEvents>, run_id: impl Into<String>) -> Self {
        Self {
            inner,
            events,
            run_id: run_id.into(),
        }
    }
}

#[async_trait]
impl<B: ActionBackend + Send> ActionBackend for EventBackend<B> {
    async fn sense(&mut self, spec: &SenseSpec) -> Result<serde_json::Value, String> {
        self.events.action(
            "step_started",
            &self.run_id,
            &format!("sense:{}", spec.target),
        );
        let result = self.inner.sense(spec).await;
        self.events.action(
            "step_finished",
            &self.run_id,
            &format!("sense:{}", spec.target),
        );
        result
    }

    async fn locate(&mut self, spec: &LocateSpec) -> Result<serde_json::Value, String> {
        self.events.action(
            "step_started",
            &self.run_id,
            &format!("locate:{}", spec.target),
        );
        let result = self.inner.locate(spec).await;
        self.events.action(
            "step_finished",
            &self.run_id,
            &format!("locate:{}", spec.target),
        );
        result
    }

    async fn act(&mut self, spec: &ActSpec) -> Result<serde_json::Value, String> {
        self.events.action(
            "step_started",
            &self.run_id,
            &format!("act:{}", spec.action),
        );
        let result = self.inner.act(spec).await;
        self.events.action(
            "step_finished",
            &self.run_id,
            &format!("act:{}", spec.action),
        );
        result
    }

    async fn invoke_skill(
        &mut self,
        skill: &str,
        args: &BTreeMap<String, String>,
    ) -> Result<serde_json::Value, String> {
        self.events
            .action("step_started", &self.run_id, &format!("skill:{skill}"));
        let result = self.inner.invoke_skill(skill, args).await;
        self.events
            .action("step_finished", &self.run_id, &format!("skill:{skill}"));
        result
    }

    async fn invoke_mcp(
        &mut self,
        server: &str,
        tool: &str,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.events.action(
            "step_started",
            &self.run_id,
            &format!("mcp:{server}:{tool}"),
        );
        let result = self.inner.invoke_mcp(server, tool, args).await;
        self.events.action(
            "step_finished",
            &self.run_id,
            &format!("mcp:{server}:{tool}"),
        );
        result
    }

    async fn notify(&mut self, message: &str) -> Result<(), String> {
        self.events.action("step_started", &self.run_id, "notify");
        let result = self.inner.notify(message).await;
        self.events.action("step_finished", &self.run_id, "notify");
        result
    }
}

// ----------------------------------------------------------------------------
// 真实后端
// ----------------------------------------------------------------------------

/// 桌面/UI 类动作（需要 computer-use 任务级审批门禁）。
pub const GATED_ACTIONS: &[&str] = &["launch", "click", "type", "key", "scroll"];

/// act 桩签名（测试注入；真实执行不使用）。
pub type ActStub = dyn Fn(&ActSpec) -> Result<serde_json::Value, String> + Send + Sync;

/// 真实后端：sense/locate/act/skill/mcp/notify 映射到既有 core 能力。
/// 安全基线：桌面动作默认门禁拒绝（无 ComputerTask 批准不执行）；文件写仅限 workspace 内。
pub struct ServerActionBackend {
    pub state: Arc<owo_agent_server::AppState>,
    /// 可注入 act 桩（测试用；Some 时 act 走桩，不触真实执行）。
    #[allow(dead_code)] // 测试桩 API（测试 crate 经 #[path] 使用，lib 目标未直接调用）
    pub act_stub: Option<Arc<ActStub>>,
}

impl ServerActionBackend {
    pub fn new(state: Arc<owo_agent_server::AppState>) -> Self {
        Self {
            state,
            act_stub: None,
        }
    }

    /// 测试/桩注入：act 动作由调用方决定结果（不触网、不动桌面）。
    #[allow(dead_code)] // 测试桩 API（测试 crate 经 #[path] 使用）
    pub fn with_act_stub(
        mut self,
        stub: impl Fn(&ActSpec) -> Result<serde_json::Value, String> + Send + Sync + 'static,
    ) -> Self {
        self.act_stub = Some(Arc::new(stub));
        self
    }

    fn audit(&self, event: &str, detail: String) {
        if let Ok(mut log) = self.state.agent.audit_log().lock() {
            log.record("workflow-backend", event, None, None, detail);
        }
    }

    /// 校验路径位于 workspace 内（拒绝 .. 越界；目标文件可尚不存在）。
    fn resolve_inside(&self, target: &str) -> Result<std::path::PathBuf, String> {
        let base = self
            .state
            .workspace
            .canonicalize()
            .map_err(|e| format!("工作区解析失败：{e}"))?;
        let joined = base.join(target);
        // write_file 的目标文件可能尚不存在：对父目录做 canonicalize 校验。
        let parent = joined
            .parent()
            .ok_or_else(|| format!("路径无父目录：{target}"))?;
        let canonical_parent = parent
            .canonicalize()
            .map_err(|e| format!("父目录不存在：{}（{e}）", parent.display()))?;
        if canonical_parent.starts_with(&base) {
            let name = joined
                .file_name()
                .ok_or_else(|| format!("路径无文件名：{target}"))?;
            Ok(canonical_parent.join(name))
        } else {
            Err(format!("目标越出工作区边界：{target}"))
        }
    }
}

#[async_trait]
impl ActionBackend for ServerActionBackend {
    async fn sense(&mut self, spec: &SenseSpec) -> Result<serde_json::Value, String> {
        match spec.target.as_str() {
            "clipboard" => {
                let clips = owo_agent_core::platform::clipboard_sequence();
                Ok(serde_json::json!({ "clipboard": clips }))
            }
            "foreground" => {
                let (app_id, title) =
                    owo_agent_core::platform::poll_foreground_app().unwrap_or_default();
                Ok(serde_json::json!({
                    "foreground": { "app": app_id, "title": title }
                }))
            }
            "files" => {
                let base = self.state.workspace.clone();
                let mut names = Vec::new();
                if let Ok(entries) = std::fs::read_dir(&base) {
                    for entry in entries.flatten() {
                        if let Some(name) = entry.file_name().to_str() {
                            names.push(name.to_string());
                        }
                    }
                }
                Ok(serde_json::json!({ "files": names, "workspace": base.to_string_lossy() }))
            }
            other => Err(format!(
                "未知感知目标：{other}（支持 clipboard|foreground|files）"
            )),
        }
    }

    async fn locate(&mut self, spec: &LocateSpec) -> Result<serde_json::Value, String> {
        let query = owo_agent_core::locate::AnchorQuery {
            name_pattern: Some(spec.target.clone()),
            ..Default::default()
        };
        let graph = self
            .state
            .scene
            .lock()
            .map_err(|e| format!("场景图锁中毒：{e}"))?;
        let result = owo_agent_core::locate::locate(&graph, &query);
        let best = result
            .best
            .as_ref()
            .map(|e| serde_json::json!({ "id": e.id, "name": e.name, "role": e.role_hint }))
            .unwrap_or(serde_json::Value::Null);
        Ok(serde_json::json!({
            "target": spec.target,
            "matched": result.best.is_some(),
            "element": best,
            "uncertainty": result.uncertainty,
        }))
    }

    async fn act(&mut self, spec: &ActSpec) -> Result<serde_json::Value, String> {
        // 可注入桩优先（测试）。
        if let Some(stub) = &self.act_stub {
            return stub(spec);
        }
        match spec.action.as_str() {
            "write_file" | "append_file" => {
                let path = self.resolve_inside(&spec.target)?;
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
                }
                let content = spec.value.clone().unwrap_or_default();
                if spec.action == "append_file" {
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                        .and_then(|mut f| {
                            use std::io::Write;
                            f.write_all(content.as_bytes())
                        })
                        .map_err(|e| format!("追加 {} 失败：{e}", path.display()))?;
                } else {
                    std::fs::write(&path, content)
                        .map_err(|e| format!("写入 {} 失败：{e}", path.display()))?;
                }
                self.audit(
                    "backend.act",
                    format!(
                        "{} {}（{} 字节）",
                        spec.action,
                        path.display(),
                        spec.value.as_ref().map(|v| v.len()).unwrap_or(0)
                    ),
                );
                Ok(serde_json::json!({ "ok": true, "action": spec.action, "target": spec.target }))
            }
            // 桌面动作：需要 computer-use 任务级审批门禁（默认 deny）。
            action if GATED_ACTIONS.contains(&action) => Err(format!(
                "动作 {action} 需要 computer-use 任务级审批门禁（无已批准任务），拒绝执行"
            )),
            other => Err(format!(
                "未知动作：{other}（支持 write_file|append_file|launch|click|type|key|scroll）"
            )),
        }
    }

    async fn invoke_skill(
        &mut self,
        skill: &str,
        args: &BTreeMap<String, String>,
    ) -> Result<serde_json::Value, String> {
        let skill_dir = self.state.data_root.join("skills").join("user").join(skill);
        if !skill_dir.is_dir() {
            return Err(format!("技能包不存在：{skill}（真实执行入口未就绪）"));
        }
        let manifest = skill_dir.join("SKILL.md");
        Ok(serde_json::json!({
            "skill": skill,
            "found": manifest.is_file(),
            "args": args,
            "executed": false,
            "reason": "技能真实执行入口待接入（v1 仅入口检查）",
        }))
    }

    async fn invoke_mcp(
        &mut self,
        server: &str,
        tool: &str,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let registry = self.state.agent.mcp_clients();
        let client = registry
            .get(server)
            .ok_or_else(|| format!("MCP 服务器未注册：{server}"))?;
        let mut client = client.lock().await;
        let output = client
            .call_tool(tool, args.clone())
            .await
            .map_err(|e| format!("MCP 工具 {server}:{tool} 失败：{e}"))?;
        Ok(serde_json::json!({ "server": server, "tool": tool, "output": output }))
    }

    async fn notify(&mut self, message: &str) -> Result<(), String> {
        self.audit("backend.notify", message.to_string());
        Ok(())
    }
}

/// 供测试与面板展示：后端选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendChoice {
    Mock,
    Real,
}

impl BackendChoice {
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("real") => Self::Real,
            _ => Self::Mock,
        }
    }
}

/// workspace 内文件安全写入（测试与后端共用）。
#[allow(dead_code)] // 测试桩 API
pub fn write_workspace_file(workspace: &Path, rel: &str, content: &str) -> Result<(), String> {
    let base = workspace
        .canonicalize()
        .map_err(|e| format!("工作区解析失败：{e}"))?;
    let joined = base.join(rel);
    if let Some(parent) = joined.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
    }
    std::fs::write(&joined, content).map_err(|e| format!("写入失败：{e}"))
}
