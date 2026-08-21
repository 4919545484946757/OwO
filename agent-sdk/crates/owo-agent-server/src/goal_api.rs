//! Goal/Plan 编排 HTTP API（Lane D Part 1）。
//!
//! - 存储：`data_root/goals/<goal_id>/goal.json、plan.json、runs/run-<run_id>.json`
//!   （Goal/GoalRunState/Plan 均为 serde 结构，persist/load 复用 core 能力）。
//! - 内置演示 worker：echo（回显输入文本）、sleep（按参数毫秒睡眠）、fail（按参数失败，演示重试/replan）。
//! - 运行注册表：`OnceLock<Mutex<HashMap<(goal_id, run_id), Arc<tokio::Mutex<GoalRunner>>>>>` 供 abort。
//! - 审计：按 data_root 键控的 `AuditLog`，写操作全部留痕，`GET /goal/{id}/audit` 暴露尾部。
//!
//! 本模块不引用 `crate::`/`super::`（AppState 全限定 `owo_agent_server::AppState`），
//! 可被测试以 `#[path = "../src/goal_api.rs"] mod goal_api;` 独立编译。

use async_trait::async_trait;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::{Json, Router};
use owo_agent_core::audit::AuditLog;
use owo_agent_core::goal::{
    Goal, GoalBudget, GoalRunState, GoalRunner, GoalStatus, RunnerConfig, Worker, WorkerRegistry,
};
use owo_agent_core::plan::{Plan, StepSpec, VerificationSpec};
use owo_agent_server::AppState;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

// R5：agent worker 作为本模块子模块编译（lib.rs 无需登记；独立编译、无 crate 引用）。
#[path = "agent_worker.rs"]
pub mod agent_worker;

// ---------- 存储路径 ----------

fn goals_dir(data_root: &Path) -> PathBuf {
    data_root.join("goals")
}

fn goal_dir(data_root: &Path, goal_id: &str) -> PathBuf {
    goals_dir(data_root).join(goal_id)
}

fn goal_file(data_root: &Path, goal_id: &str) -> PathBuf {
    goal_dir(data_root, goal_id).join("goal.json")
}

fn runs_dir(data_root: &Path, goal_id: &str) -> PathBuf {
    goal_dir(data_root, goal_id).join("runs")
}

fn not_found(detail: &str) -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_FOUND, Json(json!({ "error": detail })))
}

fn bad_request(detail: &str) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": detail })))
}

// ---------- 审计（按 data_root 键控） ----------

type AuditMap = Arc<Mutex<HashMap<PathBuf, Arc<Mutex<AuditLog>>>>>;

static AUDITS: OnceLock<AuditMap> = OnceLock::new();

fn audits() -> &'static AuditMap {
    AUDITS.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

fn audit_for(data_root: &Path) -> Arc<Mutex<AuditLog>> {
    let mut map = audits().lock().unwrap_or_else(|e| e.into_inner());
    map.entry(data_root.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(AuditLog::default())))
        .clone()
}

fn audit_record(data_root: &Path, event: &str, detail: impl Into<String>) {
    if let Ok(mut log) = audit_for(data_root).lock() {
        log.record("goal-api", event, None, None, detail);
    }
}

// ---------- 运行注册表（abort） ----------

type RunnerHandle = Arc<tokio::sync::Mutex<GoalRunner>>;
type RunnerMap = Arc<Mutex<HashMap<(String, String), RunnerHandle>>>;

static RUNNERS: OnceLock<RunnerMap> = OnceLock::new();

fn runners() -> &'static RunnerMap {
    RUNNERS.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

// ---------- 内置 worker ----------

struct EchoWorker;

#[async_trait]
impl Worker for EchoWorker {
    fn name(&self) -> &str {
        "echo"
    }

    async fn run(&self, input: &Value) -> Result<String, String> {
        Ok(input
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| input.to_string()))
    }
}

struct SleepWorker;

#[async_trait]
impl Worker for SleepWorker {
    fn name(&self) -> &str {
        "sleep"
    }

    async fn run(&self, input: &Value) -> Result<String, String> {
        let ms = input
            .get("ms")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(60_000);
        tokio::time::sleep(Duration::from_millis(ms)).await;
        Ok(format!("slept {ms}ms"))
    }
}

struct FailWorker;

#[async_trait]
impl Worker for FailWorker {
    fn name(&self) -> &str {
        "fail"
    }

    async fn run(&self, input: &Value) -> Result<String, String> {
        Err(input
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| "fail worker 注入失败".to_string()))
    }
}

fn builtin_workers(state: Option<&AppState>) -> WorkerRegistry {
    let registry = WorkerRegistry::new();
    registry.register(Arc::new(EchoWorker));
    registry.register(Arc::new(SleepWorker));
    registry.register(Arc::new(FailWorker));
    // R5：真实 Agent worker（name="agent"）。
    if let Some(state) = state {
        registry.register(Arc::new(agent_worker::AgentWorker::new(
            state.agent.clone(),
            state.workspace.clone(),
        )));
    }
    registry
}

// ---------- 请求模型 ----------

#[derive(Deserialize)]
struct CreateGoalRequest {
    objective: String,
    #[serde(default)]
    budget: Option<BudgetRequest>,
}

#[derive(Deserialize, Default)]
struct BudgetRequest {
    #[serde(default)]
    max_steps: Option<u32>,
    #[serde(default)]
    max_replans: Option<u32>,
}

#[derive(Deserialize)]
struct PlanStepInput {
    id: String,
    worker: String,
    #[serde(default)]
    deps: Vec<String>,
    #[serde(default)]
    verify: Option<VerifyInput>,
    #[serde(default)]
    max_retries: Option<u32>,
    #[serde(default)]
    input: Option<Value>,
    #[serde(default)]
    parallel: bool,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum VerifyInput {
    Text(String),
    Object { kind: String, value: Option<String> },
}

impl VerifyInput {
    fn to_spec(&self) -> VerificationSpec {
        match self {
            VerifyInput::Text(text) => VerificationSpec::OutputContains(text.clone()),
            VerifyInput::Object { kind, value } => match (kind.as_str(), value) {
                ("equals", Some(v)) => VerificationSpec::OutputEquals(v.clone()),
                ("nonempty", _) => VerificationSpec::OutputNonEmpty,
                (_, Some(v)) => VerificationSpec::OutputContains(v.clone()),
                _ => VerificationSpec::OutputNonEmpty,
            },
        }
    }
}

#[derive(Deserialize)]
struct PlanRequest {
    steps: Vec<PlanStepInput>,
}

#[derive(Deserialize, Default)]
struct RunConfigInput {
    #[serde(default)]
    parallelism: Option<usize>,
    #[serde(default)]
    allow_replan: Option<bool>,
}

// ---------- 持久化辅助 ----------

fn read_goal(data_root: &Path, goal_id: &str) -> Result<Goal, (StatusCode, Json<Value>)> {
    let path = goal_file(data_root, goal_id);
    let raw =
        std::fs::read_to_string(&path).map_err(|_| not_found(&format!("目标 {goal_id} 不存在")))?;
    serde_json::from_str(&raw).map_err(|e| bad_request(&format!("goal.json 解析失败：{e}")))
}

fn write_goal(data_root: &Path, goal: &Goal) -> Result<(), (StatusCode, Json<Value>)> {
    let dir = goal_dir(data_root, &goal.id);
    std::fs::create_dir_all(&dir).map_err(|e| bad_request(&format!("创建目标目录失败：{e}")))?;
    let raw = serde_json::to_string_pretty(goal)
        .map_err(|e| bad_request(&format!("goal 序列化失败：{e}")))?;
    std::fs::write(goal_file(data_root, &goal.id), raw)
        .map_err(|e| bad_request(&format!("goal 写入失败：{e}")))
}

fn read_plan(data_root: &Path, goal_id: &str) -> Result<Plan, (StatusCode, Json<Value>)> {
    Plan::load(&goal_dir(data_root, goal_id), "plan")
        .map_err(|_| not_found(&format!("目标 {goal_id} 尚无计划")))
}

// ---------- 路由 ----------

/// Lane D Part 1 路由：/goal/*（供主控并入 build_router）。
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/goal", axum::routing::get(list_goals).post(create_goal))
        .route("/goal/{id}", axum::routing::get(get_goal))
        .route(
            "/goal/{id}/plan",
            axum::routing::get(get_plan).post(create_plan),
        )
        .route("/goal/{id}/run", axum::routing::post(start_run))
        .route("/goal/{id}/status", axum::routing::get(goal_status))
        .route("/goal/{id}/abort", axum::routing::post(abort_goal))
        .route("/goal/{id}/audit", axum::routing::get(goal_audit))
        .route("/goal/{id}/runs", axum::routing::get(goal_runs))
        .with_state(state)
}

/// 创建目标：`POST /goal {objective, budget?}`。
async fn create_goal(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateGoalRequest>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    if request.objective.trim().is_empty() {
        return Err(bad_request("objective 不能为空"));
    }
    let mut goal = Goal::new(uuid::Uuid::new_v4().to_string(), request.objective.trim());
    goal.transition(GoalStatus::Planning);
    if let Some(budget) = request.budget {
        let mut b = GoalBudget::default();
        if let Some(max_steps) = budget.max_steps {
            b.max_steps = max_steps;
        }
        if let Some(max_replans) = budget.max_replans {
            b.max_replans = max_replans;
        }
        goal.budget = b;
    }
    write_goal(&state.data_root, &goal)?;
    audit_record(
        &state.data_root,
        "goal.create",
        format!("创建目标 {}", goal.id),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({ "ok": true, "goal": goal })),
    ))
}

/// 目标列表：`GET /goal`。
async fn list_goals(State(state): State<Arc<AppState>>) -> Json<Value> {
    let mut goals = Vec::new();
    let dir = goals_dir(&state.data_root);
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let goal_id = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            if let Ok(goal) = read_goal(&state.data_root, &goal_id) {
                goals.push(json!({
                    "id": goal.id,
                    "objective": goal.objective,
                    "status": format!("{:?}", goal.status),
                    "created_at": goal.created_at,
                    "updated_at": goal.updated_at,
                }));
            }
        }
    }
    goals.sort_by(|a, b| {
        a.get("created_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .cmp(
                b.get("created_at")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            )
    });
    Json(json!({ "goals": goals, "count": goals.len() }))
}

/// 目标详情：`GET /goal/{id}`。
async fn get_goal(
    State(state): State<Arc<AppState>>,
    AxumPath(goal_id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let goal = read_goal(&state.data_root, &goal_id)?;
    Ok(Json(json!(goal)))
}

/// 创建/替换计划：`POST /goal/{id}/plan {steps:[...]}`（环检测 + waves 预览）。
async fn create_plan(
    State(state): State<Arc<AppState>>,
    AxumPath(goal_id): AxumPath<String>,
    Json(request): Json<PlanRequest>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let goal = read_goal(&state.data_root, &goal_id)?;
    if request.steps.is_empty() {
        return Err(bad_request("steps 不能为空"));
    }
    let mut plan = Plan::new("plan".to_string(), goal_id.clone());
    let agent_model = resolve_agent_model(&request.steps);
    for step in request.steps {
        if step.id.is_empty() || step.worker.is_empty() {
            return Err(bad_request("步骤 id/worker 不能为空"));
        }
        // R5：agent 步骤必须在 input 中提供非空 prompt（预校验 → 400）。
        if step.worker == "agent" {
            let input = step.input.clone().unwrap_or(Value::Null);
            if let Err(error) = agent_worker::validate_agent_input(&input) {
                return Err(bad_request(error.as_str()));
            }
        }
        let mut spec = StepSpec::new(step.id.clone(), step.worker.clone());
        spec.depends_on = step.deps;
        spec.parallel = step.parallel;
        spec.input = step.input.unwrap_or(Value::Null);
        spec.verify = step.verify.map(|v| v.to_spec());
        spec.retries = step.max_retries.unwrap_or(0);
        plan.add_step(spec);
    }
    if let Err(error) = plan.validate() {
        return Err(bad_request(&format!("计划非法：{error}")));
    }
    let waves = plan
        .topological_waves()
        .map_err(|e| bad_request(&format!("拓扑排序失败：{e}")))?;
    plan.persist(&goal_dir(&state.data_root, &goal_id))
        .map_err(|e| bad_request(&format!("计划保存失败：{e}")))?;
    audit_record(
        &state.data_root,
        "goal.plan",
        format!("目标 {goal_id} 计划已保存（{} 步）", plan.steps.len()),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "ok": true,
            "goal_id": goal_id,
            "plan": plan,
            "waves": waves,
            "valid": true,
            "objective": goal.objective,
            "agent_model": agent_model,
        })),
    ))
}

/// R5：计划含 agent 步骤时，返回该步骤将使用的模型名（input.model → OWO_AGENT_MODEL → 缺省）。
fn resolve_agent_model(steps: &[PlanStepInput]) -> Value {
    for step in steps {
        if step.worker == "agent" {
            let input = step.input.clone().unwrap_or(Value::Null);
            return json!(agent_worker::AgentWorker::resolve_model(&input));
        }
    }
    Value::Null
}

/// 计划详情：`GET /goal/{id}/plan`。
async fn get_plan(
    State(state): State<Arc<AppState>>,
    AxumPath(goal_id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let plan = read_plan(&state.data_root, &goal_id)?;
    let waves = plan
        .topological_waves()
        .map_err(|e| bad_request(&format!("拓扑排序失败：{e}")))?;
    Ok(Json(json!({ "plan": plan, "waves": waves, "valid": true })))
}

/// 启动运行：`POST /goal/{id}/run {config?}` → 202 {run_id}。
async fn start_run(
    State(state): State<Arc<AppState>>,
    AxumPath(goal_id): AxumPath<String>,
    Json(request): Json<RunConfigInput>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let goal = read_goal(&state.data_root, &goal_id)?;
    let plan = read_plan(&state.data_root, &goal_id)?;
    let runs = runs_dir(&state.data_root, &goal_id);
    std::fs::create_dir_all(&runs).map_err(|e| bad_request(&format!("创建 runs 目录失败：{e}")))?;

    let run_id = format!("run-{uuid}", uuid = uuid::Uuid::new_v4());
    // R7（Agent 2 扩展）：WorkerPool 子进程执行默认关闭，保持进程内语义。
    let config = RunnerConfig {
        max_parallel: request.parallelism.unwrap_or(2).max(1),
        persist_dir: Some(runs),
        allow_replan: request.allow_replan.unwrap_or(true),
        use_worker_pool: false,
        worker_pool: None,
        capability_registry: None,
        capability_requirement: None,
        transport: None,
        leases: None,
    };
    let mut runner = GoalRunner::new(goal.clone(), plan, config);
    runner.attach_audit(audit_for(&state.data_root));
    runner.state.run_id = run_id.clone();
    let runner = Arc::new(tokio::sync::Mutex::new(runner));
    {
        let mut map = runners().lock().unwrap_or_else(|e| e.into_inner());
        map.insert((goal_id.clone(), run_id.clone()), Arc::clone(&runner));
    }

    let workers = builtin_workers(Some(state.as_ref()));
    let data_root = state.data_root.clone();
    let goal_id_clone = goal_id.clone();
    let run_id_clone = run_id.clone();
    tokio::spawn(async move {
        let mut guard = runner.lock().await;
        let status = guard.run(&workers).await;
        drop(guard);
        let mut map = runners().lock().unwrap_or_else(|e| e.into_inner());
        map.remove(&(goal_id_clone.clone(), run_id_clone.clone()));
        audit_record(
            &data_root,
            "goal.run.finished",
            format!("目标 {goal_id_clone} 运行 {run_id_clone} → {status:?}"),
        );
    });

    audit_record(
        &state.data_root,
        "goal.run.start",
        format!("目标 {goal_id} 启动运行 {run_id}"),
    );
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "ok": true, "goal_id": goal_id, "run_id": run_id })),
    ))
}

/// 运行状态快照：`GET /goal/{id}/status`（最新 run-*.json 的 GoalRunState）。
async fn goal_status(
    State(state): State<Arc<AppState>>,
    AxumPath(goal_id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let runs = runs_dir(&state.data_root, &goal_id);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&runs)
        .map_err(|_| not_found(&format!("目标 {goal_id} 尚无运行")))?
        .flatten()
        .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
        .map(|e| e.path())
        .collect();
    if files.is_empty() {
        return Err(not_found(&format!("目标 {goal_id} 尚无运行")));
    }
    files.sort();
    let latest = files.last().unwrap();
    let run_id = latest
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let state_value = GoalRunState::load(&runs, &run_id)
        .map_err(|e| bad_request(&format!("运行状态读取失败：{e}")))?;
    let plan = Plan::load(&goal_dir(&state.data_root, &goal_id), "plan")
        .unwrap_or_else(|_| Plan::new("plan".to_string(), goal_id.clone()));
    // R5：每步骤输出（截断 2000 字符）+ worker 名 + agent 模型名。
    let steps: Vec<Value> = state_value
        .records
        .iter()
        .map(|(step_id, record)| {
            let worker = plan
                .steps
                .iter()
                .find(|s| &s.id == step_id)
                .map(|s| s.worker.clone())
                .unwrap_or_default();
            let model = if worker == "agent" {
                let input = plan
                    .steps
                    .iter()
                    .find(|s| &s.id == step_id)
                    .map(|s| s.input.clone())
                    .unwrap_or(Value::Null);
                json!(agent_worker::AgentWorker::resolve_model(&input))
            } else {
                Value::Null
            };
            let output = record.output.clone().unwrap_or_default();
            let truncated = output.chars().count() > 2000;
            let output = output.chars().take(2000).collect::<String>();
            json!({
                "step_id": step_id,
                "worker": worker,
                "model": model,
                "status": format!("{:?}", record.status),
                "attempts": record.attempts,
                "output": output,
                "output_truncated": truncated,
                "error": record.error,
            })
        })
        .collect();
    let mut value = serde_json::to_value(&state_value)
        .map_err(|e| bad_request(&format!("运行状态序列化失败：{e}")))?;
    value["run_id"] = json!(run_id);
    value["goal_status"] = json!(format!("{:?}", state_value.goal.status));
    value["steps"] = Json(json!(steps)).0;
    Ok(Json(value))
}

/// 中止运行：`POST /goal/{id}/abort`。
async fn abort_goal(
    State(state): State<Arc<AppState>>,
    AxumPath(goal_id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    read_goal(&state.data_root, &goal_id)?;
    // 先收集 runner 句柄（std MutexGuard 不能跨 await），再逐个 abort。
    let targets: Vec<Arc<tokio::sync::Mutex<GoalRunner>>> = {
        let map = runners().lock().unwrap_or_else(|e| e.into_inner());
        map.iter()
            .filter(|((gid, _), _)| gid == &goal_id)
            .map(|(_, runner)| Arc::clone(runner))
            .collect()
    };
    let mut aborted = 0usize;
    for runner in targets {
        runner.lock().await.abort();
        aborted += 1;
    }
    audit_record(
        &state.data_root,
        "goal.abort",
        format!("目标 {goal_id} 中止 {aborted} 个运行"),
    );
    Ok(Json(
        json!({ "ok": true, "goal_id": goal_id, "aborted": aborted }),
    ))
}

/// 审计尾部：`GET /goal/{id}/audit`。
async fn goal_audit(
    State(state): State<Arc<AppState>>,
    AxumPath(goal_id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    read_goal(&state.data_root, &goal_id)?;
    let entries = {
        let log = audit_for(&state.data_root);
        let guard = log.lock().map_err(|_| bad_request("审计锁中毒"))?;
        guard
            .entries
            .iter()
            .rev()
            .take(50)
            .cloned()
            .collect::<Vec<_>>()
    };
    Ok(Json(json!({ "goal_id": goal_id, "audit": entries })))
}

/// 运行列表：`GET /goal/{id}/runs`。
async fn goal_runs(
    State(state): State<Arc<AppState>>,
    AxumPath(goal_id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    read_goal(&state.data_root, &goal_id)?;
    let runs = runs_dir(&state.data_root, &goal_id);
    let mut list = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&runs) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|x| x == "json").unwrap_or(false) {
                let run_id = path
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string();
                if let Ok(state_value) = GoalRunState::load(&runs, &run_id) {
                    list.push(json!({
                        "run_id": run_id,
                        "goal_status": format!("{:?}", state_value.goal.status),
                        "steps_taken": state_value.steps_taken,
                        "replan_count": state_value.replan_count,
                        "started_at": state_value.started_at,
                    }));
                }
            }
        }
    }
    list.sort_by(|a, b| {
        a.get("started_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .cmp(
                b.get("started_at")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            )
    });
    Ok(Json(
        json!({ "goal_id": goal_id, "runs": list, "count": list.len() }),
    ))
}
