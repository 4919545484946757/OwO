//! 记忆图谱 HTTP API（Lane：第二大脑 · 子任务 1）。
//!
//! - 结构化检索：按 app / 时间区间 / 数量过滤记忆条目。
//! - 时间线：按天分桶。
//! - 实体聚合：`normalize_summary` 词元统计 + 共现。
//! - 手动关系：`data_root/memory-graph/relations.json`（模块内维护，不触碰 core 存储）。
//! - recall 增强：附实体命中与来源 entry id。
//!
//! 数据源：`state.memory`（只读既有 MemoryStore）+ 模块内关系注册表。
//! 本模块不引用 crate::/super::（AppState 全限定），可被测试以 #[path] mod 独立编译。

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::{Json, Router};
use owo_agent_core::memory::normalize_summary;
use owo_agent_server::AppState;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

// ---------- 关系存储（模块内单例，data_root 键控） ----------

fn relations_for(data_root: &Path) -> Vec<Value> {
    let path = data_root.join("memory-graph").join("relations.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| value.get("links").cloned())
        .and_then(|links| links.as_array().cloned())
        .unwrap_or_default()
}

fn save_relations(data_root: &Path, links: &[Value]) -> Result<(), String> {
    let dir = data_root.join("memory-graph");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建 memory-graph 目录失败：{e}"))?;
    let raw = serde_json::to_string_pretty(&json!({ "links": links }))
        .map_err(|e| format!("关系序列化失败：{e}"))?;
    std::fs::write(dir.join("relations.json"), raw).map_err(|e| format!("关系写入失败：{e}"))
}

// ---------- 请求模型 ----------

#[derive(Deserialize)]
struct EntriesQuery {
    #[serde(default)]
    app: Option<String>,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct TimelineQuery {
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
}

#[derive(Deserialize)]
struct RecallQuery {
    q: String,
    #[serde(default)]
    top_k: Option<usize>,
}

#[derive(Deserialize)]
struct LinkRequest {
    a: String,
    b: String,
    relation: String,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct LinkDeleteRequest {
    a: String,
    b: String,
    relation: String,
}

// ---------- 辅助 ----------

fn bad_request(detail: &str) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": detail })))
}

fn ts_in_range(ts: &str, from: &Option<String>, to: &Option<String>) -> bool {
    if let Some(from) = from {
        if ts < from.as_str() {
            return false;
        }
    }
    if let Some(to) = to {
        if ts > to.as_str() {
            return false;
        }
    }
    true
}

// ---------- 路由 ----------

/// 记忆图谱路由（供主控并入 build_router）。
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/memory/graph/entries", axum::routing::get(graph_entries))
        .route("/memory/graph/timeline", axum::routing::get(graph_timeline))
        .route("/memory/graph/entities", axum::routing::get(graph_entities))
        .route("/memory/graph/links", axum::routing::get(graph_links))
        .route(
            "/memory/graph/link",
            axum::routing::post(graph_link_add).delete(graph_link_delete),
        )
        .route("/memory/graph/recall", axum::routing::get(graph_recall))
        .with_state(state)
}

/// 结构化检索：`GET /memory/graph/entries?app=&from=&to=&limit=`。
async fn graph_entries(
    State(state): State<Arc<AppState>>,
    Query(query): Query<EntriesQuery>,
) -> Json<Value> {
    let entries = {
        let memory = state.memory.lock().map_err(|_| "记忆锁中毒".to_string());
        match memory {
            Ok(memory) => memory.list(10_000),
            Err(e) => return Json(json!({ "error": e, "entries": [], "count": 0 })),
        }
    };
    let limit = query.limit.unwrap_or(100).min(500);
    let mut filtered: Vec<Value> = entries
        .into_iter()
        .filter(|e| {
            let app_ok = query
                .app
                .as_ref()
                .map(|app| e.app_id.contains(app.as_str()))
                .unwrap_or(true);
            app_ok && ts_in_range(&e.ts, &query.from, &query.to)
        })
        .map(|e| {
            json!({
                "id": format!("{}-{}", e.ts, e.app_id),
                "ts": e.ts,
                "app_id": e.app_id,
                "kind": e.kind,
                "summary": e.summary,
            })
        })
        .collect();
    filtered.sort_by(|a, b| {
        b.get("ts")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .cmp(a.get("ts").and_then(Value::as_str).unwrap_or_default())
    });
    filtered.truncate(limit);
    let count = filtered.len();
    Json(json!({ "entries": filtered, "count": count }))
}

/// 时间线分桶：`GET /memory/graph/timeline?from=&to=`（按天）。
async fn graph_timeline(
    State(state): State<Arc<AppState>>,
    Query(query): Query<TimelineQuery>,
) -> Json<Value> {
    let entries = {
        let memory = state.memory.lock().map_err(|_| "记忆锁中毒".to_string());
        match memory {
            Ok(memory) => memory.list(10_000),
            Err(e) => return Json(json!({ "error": e, "buckets": [] })),
        }
    };
    let mut buckets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for entry in entries {
        if !ts_in_range(&entry.ts, &query.from, &query.to) {
            continue;
        }
        let day = entry.ts.chars().take(10).collect::<String>();
        buckets
            .entry(day)
            .or_default()
            .push(format!("{}-{}", entry.ts, entry.app_id));
    }
    let buckets: Vec<Value> = buckets
        .into_iter()
        .map(|(day, ids)| json!({ "day": day, "count": ids.len(), "entry_ids": ids }))
        .collect();
    Json(json!({ "buckets": buckets, "total_days": buckets.len() }))
}

/// 实体聚合：`GET /memory/graph/entities?limit=`（词元频次 + 共现）。
async fn graph_entities(
    State(state): State<Arc<AppState>>,
    Query(query): Query<EntriesQuery>,
) -> Json<Value> {
    let entries = {
        let memory = state.memory.lock().map_err(|_| "记忆锁中毒".to_string());
        match memory {
            Ok(memory) => memory.list(10_000),
            Err(e) => return Json(json!({ "error": e, "entities": [] })),
        }
    };
    let limit = query.limit.unwrap_or(30).min(100);
    let mut frequency: HashMap<String, usize> = HashMap::new();
    let mut cooccurrence: HashMap<String, HashMap<String, usize>> = HashMap::new();
    for entry in entries {
        let tokens = normalize_summary(&entry.summary);
        for token in &tokens {
            *frequency.entry(token.clone()).or_insert(0) += 1;
            let related = cooccurrence.entry(token.clone()).or_default();
            for other in &tokens {
                if other != token {
                    *related.entry(other.clone()).or_insert(0) += 1;
                }
            }
        }
    }
    let mut ranked: Vec<(String, usize)> = frequency.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.truncate(limit);
    let entities: Vec<Value> = ranked
        .into_iter()
        .map(|(entity, count)| {
            let mut related: Vec<(String, usize)> = cooccurrence
                .get(&entity)
                .map(|map| map.iter().map(|(k, v)| (k.clone(), *v)).collect())
                .unwrap_or_default();
            related.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            related.truncate(8);
            json!({
                "entity": entity,
                "count": count,
                "related": related.iter().map(|(k, v)| json!({ "entity": k, "count": v })).collect::<Vec<_>>(),
            })
        })
        .collect();
    Json(json!({ "entities": entities, "count": entities.len() }))
}

/// 关系列表：`GET /memory/graph/links`。
async fn graph_links(State(state): State<Arc<AppState>>) -> Json<Value> {
    let links = relations_for(&state.data_root);
    Json(json!({ "links": links, "count": links.len() }))
}

/// 添加手动关系：`POST /memory/graph/link {a,b,relation,note?}`。
async fn graph_link_add(
    State(state): State<Arc<AppState>>,
    Json(request): Json<LinkRequest>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    if request.a.trim().is_empty()
        || request.b.trim().is_empty()
        || request.relation.trim().is_empty()
    {
        return Err(bad_request("a / b / relation 不能为空"));
    }
    let mut links = relations_for(&state.data_root);
    let link = json!({
        "a": request.a,
        "b": request.b,
        "relation": request.relation,
        "note": request.note,
        "ts": chrono::Utc::now().to_rfc3339(),
    });
    links.push(link.clone());
    save_relations(&state.data_root, &links).map_err(|e| bad_request(&e))?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "ok": true, "link": link, "count": links.len() })),
    ))
}

/// 删除手动关系：`DELETE /memory/graph/link {a,b,relation}`。
async fn graph_link_delete(
    State(state): State<Arc<AppState>>,
    Json(request): Json<LinkDeleteRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut links = relations_for(&state.data_root);
    let before = links.len();
    links.retain(|link| {
        !(link.get("a").and_then(Value::as_str) == Some(request.a.as_str())
            && link.get("b").and_then(Value::as_str) == Some(request.b.as_str())
            && link.get("relation").and_then(Value::as_str) == Some(request.relation.as_str()))
    });
    if links.len() == before {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "关系不存在" })),
        ));
    }
    save_relations(&state.data_root, &links).map_err(|e| bad_request(&e))?;
    Ok(Json(json!({ "ok": true, "removed": before - links.len() })))
}

/// 增强 recall：`GET /memory/graph/recall?q=&top_k=`（附实体命中与来源 id）。
async fn graph_recall(
    State(state): State<Arc<AppState>>,
    Query(query): Query<RecallQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if query.q.trim().is_empty() {
        return Err(bad_request("q 不能为空"));
    }
    let top_k = query.top_k.unwrap_or(5).min(50);
    let (hits, query_tokens) = {
        let memory = state.memory.lock().map_err(|_| bad_request("记忆锁中毒"))?;
        let query_tokens = normalize_summary(&query.q);
        let hits = memory.recall(&query.q, top_k);
        (hits, query_tokens)
    };
    let hits: Vec<Value> = hits
        .into_iter()
        .map(|entry| {
            // 中文词元无分隔：query 词元是 entry 词元的子串即视为实体命中。
            let matched: Vec<String> = query_tokens
                .iter()
                .filter(|token| entry.normalized.iter().any(|n| n.contains(token.as_str())))
                .cloned()
                .collect();
            json!({
                "id": format!("{}-{}", entry.ts, entry.app_id),
                "ts": entry.ts,
                "app_id": entry.app_id,
                "summary": entry.summary,
                "matched_entities": matched,
                "confidence": entry.confidence,
            })
        })
        .collect();
    Ok(Json(
        json!({ "query": query.q, "top_k": top_k, "hits": hits, "count": hits.len() }),
    ))
}
