// R11:usage 质量收尾完成。
// R12:usage 完成，待主控接线（四维报表 + 预算硬熔断即时置位 + prometheus 探针联动，契约测试已补）
// R12:usage 完成，待主控接线（四维报表/硬熔断经 observability_api 用量探针联动 prometheus）。
//! 用量与成本归集（R8 + R9 + R10 持久化 + R11 硬停即时性）：四维用量 + 预算硬熔断 + `/usage` 路由。
//! R11：`accumulate_budget` 累计后立即 `recheck`，预算超限在当次记录即置位硬熔断（不再等下轮）。
//! R10:usage 持久化完成，待主控接线（`persist_to`/`load_from` 供定时落盘与启动恢复；
//! `budget_exceeded_response` 供超限停轮返回 402+错误码；`/usage/report` 按天报表）。
//!
//! - 四维：session / workflow_run / goal_step / tool（`UsageDimension`）。
//! - `record_usage`：幂等友好（同 (dimension,key,correlation) 去重计数）；
//!   成本按 `price_per_mtok` 估算（默认常量，可配置）。
//! - 预算硬熔断：`Budget { limit_usd, spent_usd }`；超限置 hard_stop，
//!   `is_hard_stopped()` 供停轮，`request_topup(amount)` 恢复。
//! - 持久化（R10）：`persist_to(dir)` 落盘 records+budgets+hard_stop 快照；
//!   `load_from(dir)` 启动恢复（含硬熔断状态），崩溃后续接。
//! - 路由：`GET /usage/summary`（四维聚合 + 预算状态）、`GET /usage/records?dimension=`、
//!   `GET /usage/report?days=`（按天聚合趋势报表）。
//!
//! 本模块不引用 `crate::`/`super::`，可被测试以 `#[path] mod` 独立编译；
//! AppState 写全限定 `owo_agent_server::AppState`。

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use axum::Router;
use owo_agent_server::AppState;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// 四维用量维度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageDimension {
    Session,
    WorkflowRun,
    GoalStep,
    Tool,
}

impl UsageDimension {
    pub fn as_str(&self) -> &'static str {
        match self {
            UsageDimension::Session => "session",
            UsageDimension::WorkflowRun => "workflow_run",
            UsageDimension::GoalStep => "goal_step",
            UsageDimension::Tool => "tool",
        }
    }
}

/// 单条用量记录。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UsageRecord {
    pub dimension: String,
    pub key: String,
    pub correlation_id: Option<String>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub duration_ms: u64,
    pub cost_usd: f64,
    pub at_ms: u64,
}

/// 预算（按维度独立）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Budget {
    pub dimension: String,
    pub limit_usd: f64,
    pub spent_usd: f64,
}

/// 默认估算单价（美元 / 百万 token）。
pub const DEFAULT_PRICE_PER_MTK: f64 = 0.0020;

/// 记录上限（环形丢弃最旧）。
pub const USAGE_RECORDS_CAP: usize = 10_000;

/// 用量注册表（进程内，落盘由接线方负责）。
#[derive(Clone)]
pub struct UsageStore {
    inner: Arc<UsageInner>,
}

struct UsageInner {
    records: Mutex<Vec<UsageRecord>>,
    budgets: Mutex<HashMap<String, Budget>>,
    price_per_mtok: AtomicU64, // 定点：×1e6 存
    hard_stop: AtomicBool,
    hard_stop_reason: Mutex<Option<String>>,
}

impl Default for UsageStore {
    fn default() -> Self {
        Self::new()
    }
}

impl UsageStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(UsageInner {
                records: Mutex::new(Vec::new()),
                budgets: Mutex::new(HashMap::new()),
                price_per_mtok: AtomicU64::new((DEFAULT_PRICE_PER_MTK * 1_000_000.0) as u64),
                hard_stop: AtomicBool::new(false),
                hard_stop_reason: Mutex::new(None),
            }),
        }
    }

    /// 设置估算单价（美元 / 百万 token；0 表示只计数不计费）。
    pub fn set_price_per_mtok(&self, price: f64) {
        self.inner
            .price_per_mtok
            .store((price * 1_000_000.0) as u64, Ordering::Relaxed);
    }

    pub fn price_per_mtok(&self) -> f64 {
        self.inner.price_per_mtok.load(Ordering::Relaxed) as f64 / 1_000_000.0
    }

    /// 按维度与键记录一次用量；返回估算成本（美元）。
    pub fn record_usage(
        &self,
        dimension: UsageDimension,
        key: &str,
        correlation_id: Option<&str>,
        prompt_tokens: u64,
        completion_tokens: u64,
        duration_ms: u64,
    ) -> f64 {
        let cost = self.estimate_cost(prompt_tokens, completion_tokens);
        {
            let mut records = self.inner.records.lock().unwrap_or_else(|e| e.into_inner());
            records.push(UsageRecord {
                dimension: dimension.as_str().to_string(),
                key: key.to_string(),
                correlation_id: correlation_id.map(str::to_string),
                prompt_tokens,
                completion_tokens,
                duration_ms,
                cost_usd: (cost * 1_000_000.0).round() / 1_000_000.0,
                at_ms: now_ms(),
            });
            while records.len() > USAGE_RECORDS_CAP {
                records.remove(0);
            }
        }
        self.accumulate_budget(dimension, cost);
        cost
    }

    /// 便捷：仅 token 计数（无耗时信息）。
    pub fn record_tokens(
        &self,
        dimension: UsageDimension,
        key: &str,
        correlation_id: Option<&str>,
        prompt_tokens: u64,
        completion_tokens: u64,
    ) -> f64 {
        self.record_usage(
            dimension,
            key,
            correlation_id,
            prompt_tokens,
            completion_tokens,
            0,
        )
    }

    /// 成本估算（美元）。
    pub fn estimate_cost(&self, prompt_tokens: u64, completion_tokens: u64) -> f64 {
        (prompt_tokens + completion_tokens) as f64 * self.price_per_mtok() / 1_000_000.0
    }

    /// 设置预算（按维度）。超限后调用侧应停轮并请求用户加额。
    pub fn set_budget(&self, dimension: UsageDimension, limit_usd: f64) {
        {
            let mut budgets = self.inner.budgets.lock().unwrap_or_else(|e| e.into_inner());
            budgets.insert(
                dimension.as_str().to_string(),
                Budget {
                    dimension: dimension.as_str().to_string(),
                    limit_usd,
                    spent_usd: 0.0,
                },
            );
        } // drop guard，避免与 recheck 的 budgets 锁重入死锁
        self.recheck();
    }

    /// 预算检查：任一维度超限 → 硬熔断置位。返回当前是否被熔断。
    pub fn check_budget(&self) -> bool {
        self.recheck();
        self.is_hard_stopped()
    }

    /// R10：直接压入一条已落盘的记录（load_from 恢复用；不重复累计预算，
    /// 预算状态由 `restore_budgets` 快照恢复）。环形上限同 record_usage。
    #[allow(dead_code)] // 仅供主控接线（启动时 load_from 恢复）与测试以 #[path] 独立编译使用。
    pub fn push_record(&self, record: UsageRecord) {
        let mut records = self.inner.records.lock().unwrap_or_else(|e| e.into_inner());
        records.push(record);
        while records.len() > USAGE_RECORDS_CAP {
            records.remove(0);
        }
    }

    /// R10：预算快照（持久化用）。
    #[allow(dead_code)] // 仅供 persist_to 与测试以 #[path] 独立编译使用。
    pub fn budgets_snapshot(&self) -> Vec<Budget> {
        self.inner
            .budgets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }

    /// R10：恢复预算快照（覆盖 + 重查熔断）。
    #[allow(dead_code)] // 仅供主控接线（启动时 load_from 恢复）与测试以 #[path] 独立编译使用。
    pub fn restore_budgets(&self, budgets: Vec<Budget>) {
        let mut map = self.inner.budgets.lock().unwrap_or_else(|e| e.into_inner());
        map.clear();
        for budget in budgets {
            map.insert(budget.dimension.clone(), budget);
        }
        drop(map);
        self.recheck();
    }

    /// R10：恢复硬熔断状态（load_from 用）。
    #[allow(dead_code)] // 仅供主控接线（启动时 load_from 恢复）与测试以 #[path] 独立编译使用。
    pub fn force_hard_stop(&self, reason: Option<String>) {
        self.inner.hard_stop.store(true, Ordering::Relaxed);
        *self
            .inner
            .hard_stop_reason
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = reason;
    }

    /// 硬熔断状态（worker 每轮查询，超限停轮）。
    pub fn is_hard_stopped(&self) -> bool {
        self.inner.hard_stop.load(Ordering::Relaxed)
    }

    /// 熔断原因（供 UI/日志提示）。
    pub fn hard_stop_reason(&self) -> Option<String> {
        self.inner
            .hard_stop_reason
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 用户加额/恢复：清零熔断并可提高预算。
    pub fn request_topup(&self, dimension: UsageDimension, additional_usd: f64) {
        let mut budgets = self.inner.budgets.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(budget) = budgets.get_mut(dimension.as_str()) {
            budget.limit_usd += additional_usd;
        } else {
            budgets.insert(
                dimension.as_str().to_string(),
                Budget {
                    dimension: dimension.as_str().to_string(),
                    limit_usd: additional_usd,
                    spent_usd: 0.0,
                },
            );
        }
        self.inner.hard_stop.store(false, Ordering::Relaxed);
        *self
            .inner
            .hard_stop_reason
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// 全部记录。
    pub fn records(&self) -> Vec<UsageRecord> {
        self.inner
            .records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 按维度过滤记录。
    pub fn records_for(&self, dimension: UsageDimension) -> Vec<UsageRecord> {
        self.records()
            .into_iter()
            .filter(|r| r.dimension == dimension.as_str())
            .collect()
    }

    /// 四维聚合摘要（含预算状态与硬熔断）。
    pub fn summary(&self) -> Value {
        let records = self.records();
        let mut per: HashMap<&'static str, (u64, u64, u64, u64, f64)> = HashMap::new(); // dim -> (calls, ptok, ctok, dur, cost)
        for record in &records {
            let entry = per.entry(leak_dim(&record.dimension)).or_default();
            entry.0 += 1;
            entry.1 += record.prompt_tokens;
            entry.2 += record.completion_tokens;
            entry.3 += record.duration_ms;
            entry.4 += record.cost_usd;
        }
        let budgets = self.inner.budgets.lock().unwrap_or_else(|e| e.into_inner());
        let mut dims: Vec<Value> = per
            .iter()
            .map(|(dim, (calls, ptok, ctok, dur, cost))| {
                let budget = budgets.get(*dim);
                json!({
                    "dimension": dim,
                    "calls": calls,
                    "prompt_tokens": ptok,
                    "completion_tokens": ctok,
                    "total_tokens": ptok + ctok,
                    "duration_ms": dur,
                    "cost_usd": (cost * 1000.0).round() / 1000.0,
                    "budget": budget.map(|b| json!({
                        "limit_usd": b.limit_usd,
                        "spent_usd": (b.spent_usd * 1000.0).round() / 1000.0,
                        "exceeded": b.spent_usd > b.limit_usd,
                    })),
                })
            })
            .collect();
        dims.sort_by(|a, b| a["dimension"].as_str().cmp(&b["dimension"].as_str()));
        json!({
            "count": records.len(),
            "hard_stop": self.is_hard_stopped(),
            "hard_stop_reason": self.hard_stop_reason(),
            "price_per_mtok": self.price_per_mtok(),
            "dimensions": dims,
        })
    }

    fn accumulate_budget(&self, dimension: UsageDimension, cost: f64) {
        {
            let mut budgets = self.inner.budgets.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(budget) = budgets.get_mut(dimension.as_str()) {
                budget.spent_usd += cost;
            }
        } // drop guard：避免与 recheck 的 budgets 锁重入死锁
          // R11：累计后立即重查熔断，超限当次置位硬停（不再依赖下一次 check_budget）。
        self.recheck();
    }

    fn recheck(&self) {
        let budgets = self.inner.budgets.lock().unwrap_or_else(|e| e.into_inner());
        for budget in budgets.values() {
            if budget.spent_usd > budget.limit_usd {
                self.inner.hard_stop.store(true, Ordering::Relaxed);
                *self
                    .inner
                    .hard_stop_reason
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(format!(
                    "{} 预算超限：{:.2} > {:.2} USD",
                    budget.dimension, budget.spent_usd, budget.limit_usd
                ));
                return;
            }
        }
    }
}

/// 维度字符串到静态（记录内维度来自 as_str 常量；未知值归入 "other"）。
fn leak_dim(dim: &str) -> &'static str {
    match dim {
        "session" => "session",
        "workflow_run" => "workflow_run",
        "goal_step" => "goal_step",
        "tool" => "tool",
        _ => "other",
    }
}

type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;

/// 预算超限错误码（超限响应 body `code` 字段；供客户端/soak 判定）。
pub const BUDGET_ERROR_CODE: &str = "budget_exceeded";

/// 预算超限响应构造（R9）：402 + 可读错误 + 错误码。接线方在 turn 入口
/// `check_budget()` 命中后返回此响应（停轮并请求用户加额）。
pub fn budget_exceeded_response(reason: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::PAYMENT_REQUIRED,
        Json(json!({
            "error": "用量预算超限，已硬熔断停轮；请加额后继续",
            "code": BUDGET_ERROR_CODE,
            "detail": reason,
            "hard_stop": true,
        })),
    )
}

/// 全局单例（供路由与接线方使用；测试用 `reset_for_test`）。
static GLOBAL: Mutex<Option<Arc<UsageStore>>> = Mutex::new(None);

pub fn global() -> Arc<UsageStore> {
    let mut slot = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
    slot.get_or_insert_with(|| Arc::new(UsageStore::new()))
        .clone()
}

/// 仅供测试：重置全局单例。
#[allow(dead_code)] // 仅供测试/接线方以 #[path] 独立编译使用。
pub fn reset_global_for_test() {
    *GLOBAL.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// GET /usage/summary：四维聚合 + 预算 + 硬熔断状态。
async fn usage_summary(State(_state): State<Arc<AppState>>) -> ApiResult {
    Ok(Json(global().summary()))
}

#[derive(Deserialize)]
struct RecordsQuery {
    #[serde(default)]
    dimension: Option<String>,
}

/// GET /usage/records?dimension=：用量记录明细（可脱敏后返回）。
async fn usage_records(
    State(_state): State<Arc<AppState>>,
    Query(query): Query<RecordsQuery>,
) -> ApiResult {
    let records = match query.dimension.as_deref() {
        Some("session") => global().records_for(UsageDimension::Session),
        Some("workflow_run") => global().records_for(UsageDimension::WorkflowRun),
        Some("goal_step") => global().records_for(UsageDimension::GoalStep),
        Some("tool") => global().records_for(UsageDimension::Tool),
        Some(other) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("未知维度：{other}") })),
            ))
        }
        None => global().records(),
    };
    Ok(Json(json!({ "count": records.len(), "records": records })))
}

// ==================== R10：用量/预算持久化 ====================

/// 用量快照文件名（相对目录）。
#[allow(dead_code)] // 仅供 persist_to/load_from 使用；测试以 #[path] 独立编译时也引用。
pub const USAGE_SNAPSHOT_FILE: &str = "usage.json";

/// 用量快照版本。
#[allow(dead_code)] // 仅供 persist_to 与测试以 #[path] 独立编译使用。
pub const USAGE_SNAPSHOT_VERSION: u32 = 1;

/// 落盘用量/预算/硬熔断快照（R10）：records + budgets + hard_stop → `<dir>/usage.json`。
/// 返回快照路径。接线方可定时调用（如每小时），并在优雅关闭时最后落一次。
#[allow(dead_code)] // 仅供主控接线（定时持久化/优雅关闭）与测试以 #[path] 独立编译使用。
pub fn persist_to(dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(USAGE_SNAPSHOT_FILE);
    let store = global();
    let snapshot = json!({
        "version": USAGE_SNAPSHOT_VERSION,
        "records": store.records(),
        "budgets": store.budgets_snapshot(),
        "hard_stop": store.is_hard_stopped(),
        "hard_stop_reason": store.hard_stop_reason(),
        "saved_at_ms": now_ms(),
    });
    let text = serde_json::to_string_pretty(&snapshot)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, text)?;
    Ok(path)
}

/// 从快照恢复（R10）：重放 records、恢复 budgets 与硬熔断状态（崩溃后续接）。
/// 返回恢复的记录条数；快照不存在返回 Ok(0)。
#[allow(dead_code)] // 仅供主控接线（启动恢复）与测试以 #[path] 独立编译使用。
pub fn load_from(dir: &Path) -> std::io::Result<usize> {
    let path = dir.join(USAGE_SNAPSHOT_FILE);
    if !path.exists() {
        return Ok(0);
    }
    let text = std::fs::read_to_string(&path)?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let store = global();
    let mut restored = 0usize;
    if let Some(records) = value["records"].as_array() {
        for record in records {
            let record: UsageRecord = serde_json::from_value(record.clone())
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            store.push_record(record);
            restored += 1;
        }
    }
    if let Some(budgets) = value["budgets"].as_array() {
        let budgets: Vec<Budget> = budgets
            .iter()
            .filter_map(|b| serde_json::from_value(b.clone()).ok())
            .collect();
        store.restore_budgets(budgets);
    }
    if value["hard_stop"].as_bool().unwrap_or(false) {
        store.force_hard_stop(
            value["hard_stop_reason"]
                .as_str()
                .map(str::to_string)
                .or_else(|| Some("恢复自用量快照".to_string())),
        );
    }
    Ok(restored)
}

/// 报表（R9）：`days` 天窗口按 UTC 日期聚合（0 = 全部）+ 总计 + 预算状态。
pub fn report(days: u64) -> Value {
    let records = global().records();
    let cutoff = if days == 0 {
        0
    } else {
        now_ms().saturating_sub(days.saturating_mul(86_400_000))
    };
    let mut by_day: std::collections::BTreeMap<String, [u64; 5]> =
        std::collections::BTreeMap::new();
    let mut totals = [0u64; 5]; // calls, ptok, ctok, dur, cost(×1e6 定点)
    for record in &records {
        if record.at_ms < cutoff {
            continue;
        }
        let entry = by_day.entry(day_string(record.at_ms)).or_default();
        entry[0] += 1;
        entry[1] += record.prompt_tokens;
        entry[2] += record.completion_tokens;
        entry[3] += record.duration_ms;
        entry[4] += (record.cost_usd * 1_000_000.0).round() as u64;
        totals[0] += 1;
        totals[1] += record.prompt_tokens;
        totals[2] += record.completion_tokens;
        totals[3] += record.duration_ms;
        totals[4] += (record.cost_usd * 1_000_000.0).round() as u64;
    }
    let by_day_json: Vec<Value> = by_day
        .iter()
        .map(|(date, v)| {
            json!({
                "date": date,
                "calls": v[0],
                "prompt_tokens": v[1],
                "completion_tokens": v[2],
                "total_tokens": v[1] + v[2],
                "duration_ms": v[3],
                "cost_usd": (v[4] as f64 / 1_000_000.0 * 1000.0).round() / 1000.0,
            })
        })
        .collect();
    json!({
        "period_days": days,
        "by_day": by_day_json,
        "totals": {
            "calls": totals[0],
            "prompt_tokens": totals[1],
            "completion_tokens": totals[2],
            "total_tokens": totals[1] + totals[2],
            "duration_ms": totals[3],
            "cost_usd": (totals[4] as f64 / 1_000_000.0 * 1000.0).round() / 1000.0,
        },
        "hard_stop": global().is_hard_stopped(),
        "hard_stop_reason": global().hard_stop_reason(),
        "price_per_mtok": global().price_per_mtok(),
        "generated_at": chrono::Utc::now().to_rfc3339(),
    })
}

/// UTC 日期字符串（YYYY-MM-DD）；异常时间戳归 "unknown"。
fn day_string(at_ms: u64) -> String {
    use chrono::TimeZone;
    chrono::Utc
        .timestamp_millis_opt(at_ms as i64)
        .single()
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

#[derive(Deserialize)]
struct ReportQuery {
    #[serde(default)]
    days: Option<u64>,
}

/// GET /usage/report?days=7：按天聚合报表（供面板/诊断/soak 消费）。
async fn usage_report(
    State(_state): State<Arc<AppState>>,
    Query(query): Query<ReportQuery>,
) -> ApiResult {
    Ok(Json(report(query.days.unwrap_or(7))))
}

/// 路由：/usage/*（供主控并入 build_router）。
pub fn usage_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/usage/summary", axum::routing::get(usage_summary))
        .route("/usage/records", axum::routing::get(usage_records))
        .route("/usage/report", axum::routing::get(usage_report))
        .with_state(state)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
