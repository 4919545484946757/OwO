//! 统一自然语言/多模态命令入口（Lane：多模态入口 · 子任务 2）。
//!
//! - `POST /intent/parse {text}`：本地规则+关键词意图解析（不依赖模型），
//!   意图：run_workflow / create_goal / query_note / search_memory / desktop_action / ask_agent。
//! - `POST /command/run {mode, text?, wav_b64?}`：意图→动作路由，全部直调 core；
//!   权限默认 deny（desktop 动作默认拒绝）；voice 走 `state.stt`（不可用 503）；
//!   全部动作写审计（data_root 键控 AuditLog）。
//!
//! 本模块不引用 crate::/super::（AppState 全限定），可被测试以 #[path] mod 独立编译。

use axum::extract::State;
use axum::http::StatusCode;
use axum::{Json, Router};
use owo_agent_core::audit::AuditLog;
use owo_agent_core::goal::{Goal, GoalBudget};
use owo_agent_core::notes::{self, NoteDoc};
use owo_agent_core::plan::{Plan, StepSpec};
use owo_agent_core::skill_health::SkillHealthStore;
use owo_agent_core::workflow::{
    ActSpec, AutoApprover, MockBackend, WorkflowDefinition, WorkflowEngine, WorkflowStep,
};
use owo_agent_server::AppState;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine as _;

// ---------- 审计（按 data_root 键控） ----------

/// data_root → 审计日志（按 data_root 键控的注册表，避免 OnceLock 跨测试污染）。
type AuditRegistry = Arc<Mutex<HashMap<PathBuf, Arc<Mutex<AuditLog>>>>>;

static AUDITS: OnceLock<AuditRegistry> = OnceLock::new();

fn audit_for(data_root: &std::path::Path) -> Arc<Mutex<AuditLog>> {
    let mut map = AUDITS
        .get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.entry(data_root.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(AuditLog::default())))
        .clone()
}

fn audit_record(data_root: &std::path::Path, event: &str, detail: impl Into<String>) {
    if let Ok(mut log) = audit_for(data_root).lock() {
        log.record("intent", event, None, None, detail);
    }
}

fn bad_request(detail: &str) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": detail })))
}

// ---------- 意图解析（本地规则） ----------

#[derive(Debug, Clone, PartialEq)]
enum IntentKind {
    RunWorkflow,
    CreateGoal,
    QueryNote,
    SearchMemory,
    DesktopAction,
    AskAgent,
}

impl IntentKind {
    fn as_str(&self) -> &'static str {
        match self {
            IntentKind::RunWorkflow => "run_workflow",
            IntentKind::CreateGoal => "create_goal",
            IntentKind::QueryNote => "query_note",
            IntentKind::SearchMemory => "search_memory",
            IntentKind::DesktopAction => "desktop_action",
            IntentKind::AskAgent => "ask_agent",
        }
    }
}

#[derive(Debug, Clone)]
struct ParsedIntent {
    kind: IntentKind,
    confidence: f64,
    params: HashMap<String, String>,
}

/// 抽取引号/冒号后参数：`目标名（“xx”/：xx）`。
fn extract_quoted(text: &str, key: &str) -> Option<String> {
    if let Some(start) = text.find('“') {
        if let Some(end) = text[start + 3..].find('”') {
            let value = text[start + 3..start + 3 + end].trim().to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    if let Some(start) = text.find('"') {
        if let Some(end) = text[start + 1..].find('"') {
            let value = text[start + 1..start + 1 + end].trim().to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    let pattern = format!("{key}：");
    if let Some(start) = text.find(&pattern) {
        let tail = &text[start + pattern.len()..];
        let value: String = tail
            .chars()
            .take_while(|c| {
                !c.is_whitespace() && *c != '，' && *c != '。' && *c != ',' && *c != '.'
            })
            .collect();
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

/// 取第一个全角/半角冒号后的词（通用参数抽取）。
fn extract_after_colon(text: &str) -> Option<String> {
    for marker in ["：", ":"] {
        if let Some(start) = text.find(marker) {
            let tail = &text[start + marker.len()..];
            let value: String = tail
                .chars()
                .take_while(|c| {
                    !c.is_whitespace() && *c != '，' && *c != '。' && *c != ',' && *c != '.'
                })
                .collect();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

fn contains_any(text: &str, keywords: &[&str]) -> bool {
    keywords.iter().any(|k| text.contains(k))
}

/// 本地规则意图解析（纯函数，可测）。
fn parse_intent(text: &str) -> ParsedIntent {
    let text = text.trim().to_lowercase();
    let mut params = HashMap::new();

    if contains_any(
        &text,
        &[
            "运行工作流",
            "执行流程",
            "跑工作流",
            "run workflow",
            "run_workflow",
        ],
    ) {
        if let Some(name) = extract_quoted(&text, "工作流")
            .or_else(|| extract_quoted(&text, "workflow"))
            .or_else(|| extract_after_colon(&text))
        {
            params.insert("workflow".to_string(), name);
        }
        return ParsedIntent {
            kind: IntentKind::RunWorkflow,
            confidence: 0.95,
            params,
        };
    }
    if contains_any(
        &text,
        &["创建目标", "新建目标", "目标：", "create goal", "new goal"],
    ) {
        if let Some(objective) =
            extract_quoted(&text, "objective").or_else(|| extract_quoted(&text, "目标"))
        {
            params.insert("objective".to_string(), objective);
        } else {
            params.insert("objective".to_string(), text.clone());
        }
        return ParsedIntent {
            kind: IntentKind::CreateGoal,
            confidence: 0.9,
            params,
        };
    }
    if contains_any(
        &text,
        &[
            "查笔记",
            "搜索笔记",
            "找笔记",
            "query note",
            "search note",
            "笔记",
        ],
    ) {
        if let Some(keyword) =
            extract_quoted(&text, "笔记").or_else(|| extract_quoted(&text, "note"))
        {
            params.insert("query".to_string(), keyword);
        } else {
            params.insert("query".to_string(), text.clone());
        }
        return ParsedIntent {
            kind: IntentKind::QueryNote,
            confidence: 0.88,
            params,
        };
    }
    if contains_any(
        &text,
        &["回忆", "记忆", "搜索记忆", "recall", "search memory"],
    ) {
        if let Some(query) =
            extract_quoted(&text, "记忆").or_else(|| extract_quoted(&text, "query"))
        {
            params.insert("query".to_string(), query);
        } else {
            params.insert("query".to_string(), text.clone());
        }
        return ParsedIntent {
            kind: IntentKind::SearchMemory,
            confidence: 0.85,
            params,
        };
    }
    if contains_any(
        &text,
        &["点击", "打开应用", "启动", "输入", "desktop", "click"],
    ) {
        if let Some(target) = extract_quoted(&text, "target").or_else(|| extract_after_colon(&text))
        {
            params.insert("target".to_string(), target);
        }
        return ParsedIntent {
            kind: IntentKind::DesktopAction,
            confidence: 0.7,
            params,
        };
    }
    ParsedIntent {
        kind: IntentKind::AskAgent,
        confidence: 0.4,
        params,
    }
}

// ---------- 命令动作路由 ----------

fn demo_workflow_definition(name: &str) -> WorkflowDefinition {
    WorkflowDefinition {
        id: format!("wf-{name}"),
        name: name.to_string(),
        version: 1,
        triggers: Vec::new(),
        permissions: Vec::new(),
        preconditions: Vec::new(),
        rollback_points: Vec::new(),
        steps: vec![WorkflowStep::Act {
            id: "demo-act".to_string(),
            scope: "demo".to_string(),
            spec: ActSpec {
                action: "write_file".to_string(),
                target: "demo-output.txt".to_string(),
                value: Some(format!("demo flow: {name}")),
            },
        }],
        max_steps: 10,
        subflow_depth_limit: 3,
    }
}

async fn route_action(
    state: &AppState,
    intent: &ParsedIntent,
) -> Result<Value, (StatusCode, Json<Value>)> {
    match intent.kind {
        IntentKind::RunWorkflow => {
            let name = intent
                .params
                .get("workflow")
                .cloned()
                .unwrap_or_else(|| "demo".to_string());
            let work_root = state.data_root.join("intent-workflows").join(&name);
            std::fs::create_dir_all(&work_root)
                .map_err(|e| bad_request(&format!("创建工作目录失败：{e}")))?;
            let flow = demo_workflow_definition(&name);
            let engine = WorkflowEngine::new(
                flow.clone(),
                HashMap::new(),
                Box::new(MockBackend::new(work_root.clone())),
                Box::new(AutoApprover { approve: false }),
                SkillHealthStore::new(None),
                work_root.clone(),
            );
            let mut engine = engine;
            let outcome = tokio::time::timeout(std::time::Duration::from_secs(30), engine.run())
                .await
                .map_err(|_| bad_request("工作流演示执行超时"))?
                .map_err(|e| bad_request(&format!("工作流执行失败：{e}")))?;
            Ok(json!({
                "workflow": name,
                "state": format!("{:?}", outcome.state),
                "steps": outcome.steps,
                "rollback_to": outcome.rollback_to,
            }))
        }
        IntentKind::CreateGoal => {
            let objective = intent
                .params
                .get("objective")
                .cloned()
                .unwrap_or_else(|| "语音创建的目标".to_string());
            let goal_id = uuid::Uuid::new_v4().to_string();
            let mut goal = Goal::new(goal_id.clone(), objective.clone());
            goal.budget = GoalBudget::default();
            let dir = state.data_root.join("goals").join(&goal_id);
            std::fs::create_dir_all(&dir)
                .map_err(|e| bad_request(&format!("创建目标目录失败：{e}")))?;
            let raw =
                serde_json::to_string_pretty(&goal).map_err(|e| bad_request(&e.to_string()))?;
            std::fs::write(dir.join("goal.json"), raw)
                .map_err(|e| bad_request(&format!("写入目标失败：{e}")))?;
            // 预置一个演示计划（echo 一步），与 goal_api 存储约定兼容。
            let mut plan = Plan::new("plan".to_string(), goal_id.clone());
            plan.add_step(StepSpec::new("step1", "echo"));
            plan.persist(&dir)
                .map_err(|e| bad_request(&format!("写入计划失败：{e}")))?;
            Ok(json!({ "goal_id": goal_id, "objective": objective }))
        }
        IntentKind::QueryNote => {
            let query = intent.params.get("query").cloned().unwrap_or_default();
            let notes_dir = state.data_root.join("notes");
            let mut hits: Vec<Value> = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&notes_dir) {
                for entry in entries.flatten() {
                    let dir = entry.path();
                    if !dir.is_dir() {
                        continue;
                    }
                    let Ok(doc) = notes::load_doc(&dir) else {
                        continue;
                    };
                    let title = doc.title.clone();
                    let text = doc_text(&doc);
                    let score = match_score(&query, &title, &text);
                    if score > 0 {
                        hits.push(json!({
                            "id": doc.id,
                            "title": title,
                            "score": score,
                        }));
                    }
                }
            }
            hits.sort_by(|a, b| b["score"].as_u64().cmp(&a["score"].as_u64()));
            hits.truncate(10);
            Ok(json!({ "query": query, "hits": hits, "count": hits.len() }))
        }
        IntentKind::SearchMemory => {
            let query = intent.params.get("query").cloned().unwrap_or_default();
            let hits = {
                let memory = state.memory.lock().map_err(|_| bad_request("记忆锁中毒"))?;
                memory.recall(&query, 5)
            };
            let hits: Vec<Value> = hits
                .into_iter()
                .map(|e| json!({ "ts": e.ts, "app_id": e.app_id, "summary": e.summary }))
                .collect();
            Ok(json!({ "query": query, "hits": hits, "count": hits.len() }))
        }
        IntentKind::DesktopAction => {
            // 权限默认 deny：真实桌面动作默认拒绝，需显式审批通道（本轮不提供）。
            audit_record(
                &state.data_root,
                "command.desktop.denied",
                "桌面动作默认拒绝（权限 deny）",
            );
            Ok(json!({
                "blocked": true,
                "reason": "桌面动作默认拒绝：需显式审批通道",
                "target": intent.params.get("target"),
            }))
        }
        IntentKind::AskAgent => {
            if std::env::var("OPENAI_API_KEY")
                .ok()
                .filter(|k| !k.is_empty())
                .is_none()
            {
                return Err(bad_request("ask_agent 需要 OPENAI_API_KEY（未配置）"));
            }
            let prompt = intent
                .params
                .values()
                .cloned()
                .collect::<Vec<_>>()
                .join("；");
            let prompt = if prompt.is_empty() {
                "请用一句话总结这段话的含义。".to_string()
            } else {
                prompt
            };
            let model =
                std::env::var("OWO_AGENT_MODEL").unwrap_or_else(|_| "gpt-4.1-mini".to_string());
            let result = state
                .agent
                .run_subagent(&state.workspace, &model, &prompt, true)
                .await
                .map_err(|e| bad_request(&format!("子代理执行失败：{e}")))?;
            Ok(json!({ "model": model, "reply": result }))
        }
    }
}

/// 块树文本拼接（从根块递归）。
fn doc_text(doc: &NoteDoc) -> String {
    fn walk(doc: &NoteDoc, block: &notes::Block, out: &mut Vec<String>) {
        out.push(notes::block_text(doc, block));
        for child in &block.children {
            if let Some(child_block) = doc.blocks.get(child) {
                walk(doc, child_block, out);
            }
        }
    }
    let mut out = Vec::new();
    if let Some(root) = doc.blocks.get(&doc.root) {
        walk(doc, root, &mut out);
    }
    out.join("\n")
}

fn match_score(query: &str, title: &str, text: &str) -> u64 {
    let query = query.to_lowercase();
    let mut score = 0u64;
    if title.to_lowercase().contains(&query) {
        score += 10;
    }
    if text.to_lowercase().contains(&query) {
        score += 3;
    }
    score
}

// ---------- 请求模型与路由 ----------

#[derive(Deserialize)]
struct ParseRequest {
    text: String,
}

#[derive(Deserialize)]
struct CommandRequest {
    #[serde(default)]
    mode: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    wav_b64: Option<String>,
}

/// 统一命令入口路由（供主控并入 build_router）。
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/intent/parse", axum::routing::post(intent_parse))
        .route("/command/run", axum::routing::post(command_run))
        .route("/command/audit", axum::routing::get(command_audit))
        .with_state(state)
}

/// 意图解析：`POST /intent/parse {text}`。
async fn intent_parse(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ParseRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if request.text.trim().is_empty() {
        return Err(bad_request("text 不能为空"));
    }
    let parsed = parse_intent(&request.text);
    audit_record(
        &state.data_root,
        "intent.parse",
        format!("{:?} {}", parsed.kind, request.text),
    );
    Ok(Json(json!({
        "intent": parsed.kind.as_str(),
        "confidence": parsed.confidence,
        "params": parsed.params,
        "text": request.text,
    })))
}

/// 命令执行：`POST /command/run {mode, text?, wav_b64?}`。
async fn command_run(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CommandRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mode = request.mode.as_str();
    let mut text = request.text.clone().unwrap_or_default();

    if mode == "voice" {
        let wav_b64 = request
            .wav_b64
            .as_deref()
            .ok_or_else(|| bad_request("voice 模式需要 wav_b64"))?;
        let wav = base64::engine::general_purpose::STANDARD
            .decode(wav_b64)
            .map_err(|e| bad_request(&format!("wav_b64 解码失败：{e}")))?;
        if wav.is_empty() || wav.len() < 44 || &wav[..4] != b"RIFF" {
            return Err(bad_request("wav_b64 不是合法 WAV"));
        }
        let stt = state.stt.lock().map_err(|_| bad_request("stt 锁中毒"))?;
        if !stt.is_ready() {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "stt not ready" })),
            ));
        }
        let wav_path =
            std::env::temp_dir().join(format!("owo-intent-{}.wav", uuid::Uuid::new_v4()));
        std::fs::write(&wav_path, &wav)
            .map_err(|e| bad_request(&format!("写临时 wav 失败：{e}")))?;
        let outcome = stt
            .transcribe_wav(&wav_path)
            .map_err(|e| bad_request(&format!("转写失败：{e}")))?;
        let _ = std::fs::remove_file(&wav_path);
        text = outcome.text;
    }

    let parsed = parse_intent(&text);
    audit_record(
        &state.data_root,
        "command.run",
        format!("mode={mode} intent={} text={}", parsed.kind.as_str(), text),
    );
    let results = route_action(&state, &parsed).await?;
    Ok(Json(json!({
        "intent": parsed.kind.as_str(),
        "confidence": parsed.confidence,
        "params": parsed.params,
        "text": text,
        "mode": mode,
        "results": results,
    })))
}

/// 命令审计尾部：`GET /command/audit`。
async fn command_audit(State(state): State<Arc<AppState>>) -> Json<Value> {
    let entries = {
        let log = audit_for(&state.data_root);
        let guard = log.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .entries
            .iter()
            .rev()
            .take(50)
            .cloned()
            .collect::<Vec<_>>()
    };
    Json(json!({ "audit": entries }))
}
