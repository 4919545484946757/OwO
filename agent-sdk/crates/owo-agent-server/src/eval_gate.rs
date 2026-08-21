//! eval 护栏（R5 Agent 3 子任务 1）：`/eval/gate/*`。
//!
//! - `POST /eval/gate/run {suite?, model?}`：无 OPENAI_API_KEY 返回
//!   `200 {skipped: true, reason}`；有凭据则 `run_suite` 并把报告落盘
//!   `data_root/eval/reports/<UTC 时间戳>.json`。
//! - `GET /eval/gate/report`：最新一份报告。
//! - `GET /eval/gate/reports`：历史报告列表（按时间倒序）。
//!
//! 本模块不引用 `crate::`/`super::`，可被测试以 `#[path] mod` 独立编译；
//! AppState 一律写全限定 `owo_agent_server::AppState`。

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use axum::Router;
use owo_agent_core::eval::{builtin_suite, eval_suite_path, run_suite};
use owo_agent_core::gateway::{OpenAiCompatibleConfig, OpenAiCompatibleProvider};
use owo_agent_core::ModelProvider;
use owo_agent_server::AppState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn err(status: StatusCode, message: impl Into<String>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": message.into() })))
}

/// 报告目录：`<data_root>/eval/reports`。
fn reports_dir(data_root: &std::path::Path) -> PathBuf {
    data_root.join("eval").join("reports")
}

/// 运行参数：suite 可为内置名或自定义路径（与 CLI `--suite` 语义一致）。
#[derive(Deserialize)]
struct RunRequest {
    #[serde(default)]
    suite: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

/// 盘面报告（含时间戳与摘要字段，供面板直接渲染）。
#[derive(Serialize)]
struct StoredReport {
    file: String,
    timestamp: String,
    suite: String,
    total: usize,
    passed: usize,
    pass_rate: f64,
    total_duration_ms: u64,
    failures: Vec<Value>,
    model: String,
}

async fn run_gate(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RunRequest>,
) -> ApiResult {
    // 无凭据优雅降级：明确 skipped 与原因，不 panic、不判失败。
    if std::env::var("OPENAI_API_KEY")
        .map(|k| k.trim().is_empty())
        .unwrap_or(true)
    {
        let reason = if std::env::var("OPENAI_BASE_URL")
            .map(|url| url.starts_with("http://") || url.starts_with("https://"))
            .unwrap_or(false)
        {
            "OPENAI_BASE_URL 指向本地端点但 OPENAI_API_KEY 未配置".to_string()
        } else {
            "缺少 OPENAI_API_KEY，真实模型 eval 跳过".to_string()
        };
        return Ok(Json(json!({ "skipped": true, "reason": reason })));
    }

    // 加载套件：默认内置；也可给路径（eval_suite_path 只读）。
    let suite = match &request.suite {
        Some(path) if !path.is_empty() => eval_suite_path(&PathBuf::from(path))
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, format!("套件加载失败：{path}")))?,
        _ => builtin_suite(),
    };
    let model = request
        .model
        .clone()
        .or_else(|| std::env::var("OWO_AGENT_MODEL").ok())
        .unwrap_or_else(|| "gpt-4.1-mini".to_string());

    let config = OpenAiCompatibleConfig::from_env().map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    let provider: Arc<dyn ModelProvider> = Arc::new(
        OpenAiCompatibleProvider::new(config)
            .map_err(|e| err(StatusCode::BAD_REQUEST, format!("模型初始化失败：{e}")))?,
    );

    let report = run_suite(provider, &model, &suite).await;
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let file = format!("{timestamp}.json");
    let dir = reports_dir(&state.data_root);
    std::fs::create_dir_all(&dir).map_err(|e| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("创建报告目录失败：{e}"),
        )
    })?;

    let failures: Vec<Value> = report
        .cases
        .iter()
        .filter(|c| !c.passed)
        .map(|c| {
            json!({
                "name": c.name,
                "error": c.error,
                "output": c.output.chars().take(500).collect::<String>(),
                "duration_ms": c.duration_ms,
            })
        })
        .collect();

    let stored = StoredReport {
        file: file.clone(),
        timestamp: timestamp.clone(),
        suite: report.suite.clone(),
        total: report.total,
        passed: report.passed,
        pass_rate: report.pass_rate,
        total_duration_ms: report.total_duration_ms,
        failures: failures.clone(),
        model: model.clone(),
    };
    let json_pretty = serde_json::to_string_pretty(&stored)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    std::fs::write(dir.join(&file), json_pretty).map_err(|e| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("报告落盘失败：{e}"),
        )
    })?;

    Ok(Json(json!({
        "ok": true,
        "report": stored,
    })))
}

/// 读取一份报告文件为 Value。
fn read_report_file(path: &std::path::Path) -> Option<Value> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<Value>(&content).ok()
}

/// 列出历史报告（按文件名倒序，即最新在前）。
fn list_report_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().map(|e| e == "json").unwrap_or(false))
            .collect(),
        Err(_) => Vec::new(),
    };
    files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    files
}

async fn latest_report(State(state): State<Arc<AppState>>) -> ApiResult {
    let dir = reports_dir(&state.data_root);
    let files = list_report_files(&dir);
    let file = files.first().ok_or_else(|| {
        err(
            StatusCode::NOT_FOUND,
            "暂无 eval 报告（先 POST /eval/gate/run）",
        )
    })?;
    let report = read_report_file(file)
        .ok_or_else(|| err(StatusCode::INTERNAL_SERVER_ERROR, "最新报告读取失败"))?;
    Ok(Json(json!({
        "file": file.file_name().unwrap_or_default().to_string_lossy(),
        "report": report,
    })))
}

async fn report_history(State(state): State<Arc<AppState>>) -> ApiResult {
    let dir = reports_dir(&state.data_root);
    let mut items: Vec<Value> = Vec::new();
    for file in list_report_files(&dir) {
        if let Some(report) = read_report_file(&file) {
            items.push(json!({
                "file": file.file_name().unwrap_or_default().to_string_lossy(),
                "timestamp": report.get("timestamp").cloned().unwrap_or(Value::Null),
                "suite": report.get("suite").cloned().unwrap_or(Value::Null),
                "passed": report.get("passed").cloned().unwrap_or(Value::Null),
                "total": report.get("total").cloned().unwrap_or(Value::Null),
                "pass_rate": report.get("pass_rate").cloned().unwrap_or(Value::Null),
                "total_duration_ms": report.get("total_duration_ms").cloned().unwrap_or(Value::Null),
                "model": report.get("model").cloned().unwrap_or(Value::Null),
            }));
        }
    }
    Ok(Json(json!({ "count": items.len(), "reports": items })))
}

/// 路由：/eval/gate/*（供主控并入 build_router）。
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/eval/gate/run", axum::routing::post(run_gate))
        .route("/eval/gate/report", axum::routing::get(latest_report))
        .route("/eval/gate/reports", axum::routing::get(report_history))
        .with_state(state)
}
