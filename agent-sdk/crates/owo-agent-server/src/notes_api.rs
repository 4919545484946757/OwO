//! 笔记 HTTP API（Lane A，第四轮 HTTP/UI 集成轮）。
//!
//! 只新建本文件；禁止修改任何既有文件（lib.rs/Cargo.toml/core 等由主控统一收尾）。
//! 独立编译约束：本模块不使用 `crate::`/`super::`；引用 server 类型一律写全限定名
//! `owo_agent_server::AppState`，保证测试能以 `#[path = "../src/notes_api.rs"] mod notes_api;`
//! 方式独立编译。
//!
//! 存储：按 `AppState.data_root` 键控的模块内单例注册表（不允许给 AppState 加字段）。
//! `<data_root>/notes/<id>/doc.json`（save_doc/load_doc）+ `index.json` 清单 +
//! `<id>/fts.db`（每文档 FTS5 索引，写操作后重索引该文档；搜索遍历合并）。
//! 写操作一律留审计（复用 owo_agent_core::AuditLog，经 `state.agent.audit_log()`）。

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use owo_agent_core::{
    add_block, block_text, doc_title, doc_to_md, insert_child, load_doc, md_to_doc, move_block,
    new_doc, remove_block, sanitize_html, save_doc, walk, Block, BlockId, BlockKind,
    CanvasBlockData, CanvasNote, CanvasRect, NoteDoc, NoteIndex, NoteIndexer, SearchHit,
};

// ----------------------------------------------------------------------------
// 存储：data_root 键控注册表（模块内单例）
// ----------------------------------------------------------------------------

static STORES: OnceLock<std::sync::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<NoteStore>>>>> =
    OnceLock::new();

fn stores() -> &'static std::sync::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<NoteStore>>>> {
    STORES.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn store_for(data_root: &Path) -> Arc<tokio::sync::Mutex<NoteStore>> {
    let mut map = stores().lock().unwrap();
    map.entry(data_root.to_path_buf())
        .or_insert_with(|| {
            Arc::new(tokio::sync::Mutex::new(NoteStore::new(
                data_root.join("notes"),
            )))
        })
        .clone()
}

/// 笔记存储：文档目录 + 清单 + 每文档全文索引。
struct NoteStore {
    root: PathBuf,
    /// doc_id → 索引器（FTS5，db 位于 <id>/fts.db）。
    indexers: HashMap<String, NoteIndexer>,
}

impl NoteStore {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            indexers: HashMap::new(),
        }
    }

    fn doc_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    fn ensure_root(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.root).map_err(|e| format!("创建笔记目录失败：{e}"))
    }

    fn list(&mut self) -> Result<Vec<Value>, String> {
        self.ensure_root()?;
        // index.json 清单（损坏/缺失时按目录扫描重建）
        let index_path = self.root.join("index.json");
        if let Ok(content) = std::fs::read_to_string(&index_path) {
            if let Ok(list) = serde_json::from_str::<Vec<Value>>(&content) {
                return Ok(list);
            }
        }
        let mut list = Vec::new();
        for entry in std::fs::read_dir(&self.root).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let dir = entry.path();
            if !dir.is_dir() || dir.file_name().and_then(|n| n.to_str()) == Some("index.json") {
                continue;
            }
            if let Ok(doc) = load_doc(&dir) {
                list.push(json!({
                    "id": doc.id,
                    "title": doc.title,
                    "updated_at": doc.updated_at,
                }));
            }
        }
        list.sort_by(|a, b| {
            a["updated_at"]
                .as_str()
                .unwrap_or("")
                .cmp(b["updated_at"].as_str().unwrap_or(""))
        });
        Ok(list)
    }

    fn write_index(&self, list: &[Value]) -> Result<(), String> {
        let content = serde_json::to_string_pretty(list).map_err(|e| e.to_string())?;
        std::fs::write(self.root.join("index.json"), content).map_err(|e| e.to_string())
    }

    fn load(&self, id: &str) -> Result<NoteDoc, String> {
        let dir = self.doc_dir(id);
        if !dir.is_dir() {
            return Err(format!("笔记不存在：{id}"));
        }
        load_doc(&dir)
    }

    /// 持久化 + 更新清单 + 重索引该文档。
    fn persist(&mut self, doc: &NoteDoc) -> Result<(), String> {
        self.ensure_root()?;
        save_doc(doc, &self.doc_dir(&doc.id))?;
        let list = self.list()?;
        let list: Vec<Value> = list
            .into_iter()
            .map(|item| {
                if item["id"].as_str() == Some(doc.id.as_str()) {
                    json!({
                        "id": doc.id,
                        "title": doc.title,
                        "updated_at": doc.updated_at,
                    })
                } else {
                    item
                }
            })
            .collect();
        let list = if list
            .iter()
            .any(|item| item["id"].as_str() == Some(doc.id.as_str()))
        {
            list
        } else {
            let mut list = list;
            list.push(json!({
                "id": doc.id,
                "title": doc.title,
                "updated_at": doc.updated_at,
            }));
            list
        };
        self.write_index(&list)?;
        self.reindex(doc)
    }

    /// 重建文档索引（FTS5：db 位于 <id>/fts.db）。
    fn reindex(&mut self, doc: &NoteDoc) -> Result<(), String> {
        let db_path = self.doc_dir(&doc.id).join("fts.db");
        let mut indexer = NoteIndexer::fts(&db_path).map_err(|e| e.to_string())?;
        indexer.index_doc(doc)?;
        self.indexers.insert(doc.id.clone(), indexer);
        Ok(())
    }

    /// 跨所有文档检索（遍历每文档索引合并）。
    fn search(&mut self, query: &str) -> Result<Vec<SearchHit>, String> {
        let mut hits: Vec<SearchHit> = Vec::new();
        let ids: Vec<String> = self
            .list()?
            .iter()
            .filter_map(|i| i["id"].as_str().map(str::to_string))
            .collect();
        for id in ids {
            let fts_path = self.doc_dir(&id).join("fts.db");
            let indexer = self.indexers.entry(id.clone()).or_insert_with(|| {
                NoteIndexer::fts(&fts_path).unwrap_or_else(|_| NoteIndexer::in_memory())
            });
            hits.extend(indexer.search(query));
        }
        hits.sort_by(|a, b| a.doc_id.cmp(&b.doc_id).then(a.block_id.cmp(&b.block_id)));
        hits.dedup();
        Ok(hits)
    }

    fn delete(&mut self, id: &str) -> Result<(), String> {
        let dir = self.doc_dir(id);
        if !dir.is_dir() {
            return Err(format!("笔记不存在：{id}"));
        }
        // 先释放 FTS 索引连接（SQLite 句柄在 Windows 上会阻止删除目录）。
        self.indexers.remove(id);
        std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
        let list = self.list()?;
        let list: Vec<Value> = list
            .into_iter()
            .filter(|item| item["id"].as_str() != Some(id))
            .collect();
        self.write_index(&list)
    }
}

// ----------------------------------------------------------------------------
// 请求/响应模型
// ----------------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateNoteRequest {
    title: String,
    #[serde(default)]
    markdown: Option<String>,
}

#[derive(Deserialize)]
struct ReplaceNoteRequest {
    #[serde(default)]
    title: Option<String>,
    /// 完整块表（BTreeMap 序列化的对象）。缺省仅改标题。
    #[serde(default)]
    blocks: Option<serde_json::Map<String, Value>>,
}

#[derive(Deserialize)]
struct AddBlockRequest {
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    after: Option<String>,
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    data: Option<Value>,
}

#[derive(Deserialize)]
struct UpdateBlockRequest {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    data: Option<Value>,
}

#[derive(Deserialize)]
struct MoveBlockRequest {
    block_id: String,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    after: Option<String>,
}

#[derive(Deserialize)]
struct ImportRequest {
    title: String,
    markdown: String,
}

// ----------------------------------------------------------------------------
// 块 kind 解析（text/data → BlockKind）
// ----------------------------------------------------------------------------

fn parse_block_kind(kind: &str, text: Option<String>, data: &Value) -> Result<BlockKind, String> {
    let text = text.unwrap_or_default();
    match kind {
        "paragraph" => Ok(BlockKind::Paragraph { text }),
        "heading" => {
            let level = data
                .get("level")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .clamp(1, 6) as u8;
            Ok(BlockKind::Heading { level, text })
        }
        "list" => {
            let ordered = data
                .get("ordered")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Ok(BlockKind::List { ordered })
        }
        "list_item" => Ok(BlockKind::ListItem { text }),
        "code" => {
            let language = data
                .get("language")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Ok(BlockKind::Code { language, text })
        }
        "table" => {
            let rows: Vec<Vec<String>> = data
                .get("rows")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(Value::as_array)
                        .map(|row| {
                            row.iter()
                                .map(|cell| cell.as_str().unwrap_or("").to_string())
                                .collect()
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(BlockKind::Table { rows })
        }
        "image" => {
            let src = data
                .get("src")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Ok(BlockKind::Image { src, alt: text })
        }
        "file" => {
            let path = data
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let mime = data
                .get("mime")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Ok(BlockKind::File { path, mime })
        }
        "quote" => Ok(BlockKind::Quote { text }),
        "html" => Ok(BlockKind::HtmlEmbed {
            html: sanitize_html(&text),
        }),
        "canvas" => {
            let data = parse_canvas(data);
            Ok(BlockKind::Canvas { data })
        }
        "ai" => {
            let model = data
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let prompt = data
                .get("prompt")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Ok(BlockKind::AiGenerated {
                model,
                prompt,
                text,
            })
        }
        other => Err(format!("未知块类型：{other}")),
    }
}

fn parse_canvas(data: &Value) -> CanvasBlockData {
    let data = data.get("canvas").unwrap_or(data);
    let rects = data
        .get("rects")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(CanvasRect {
                        id: item.get("id")?.as_str()?.to_string(),
                        x: item.get("x").and_then(Value::as_f64).unwrap_or(0.0),
                        y: item.get("y").and_then(Value::as_f64).unwrap_or(0.0),
                        w: item.get("w").and_then(Value::as_f64).unwrap_or(50.0),
                        h: item.get("h").and_then(Value::as_f64).unwrap_or(30.0),
                        layer: item
                            .get("layer")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let notes = data
        .get("notes")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(CanvasNote {
                        id: item.get("id")?.as_str()?.to_string(),
                        x: item.get("x").and_then(Value::as_f64).unwrap_or(0.0),
                        y: item.get("y").and_then(Value::as_f64).unwrap_or(0.0),
                        text: item
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let layers = data
        .get("layers")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    CanvasBlockData {
        rects,
        notes,
        layers,
    }
}

/// 校验完整块树：root 存在、children 引用存在、无孤儿块。
fn validate_doc(doc: &NoteDoc) -> Result<(), String> {
    if !doc.blocks.contains_key(&doc.root) {
        return Err("缺少根块".to_string());
    }
    for (id, block) in &doc.blocks {
        for child in &block.children {
            if !doc.blocks.contains_key(child) {
                return Err(format!("块 {id} 引用了不存在的子块：{child}"));
            }
        }
    }
    let reachable = walk(doc, &doc.root).len();
    if reachable != doc.blocks.len() {
        return Err(format!(
            "块树不完整：可达 {reachable} 块，实际 {} 块（存在孤儿）",
            doc.blocks.len()
        ));
    }
    Ok(())
}

/// 定位 after 块所在父与下一索引。
fn locate_after(doc: &NoteDoc, after: &str) -> Result<(String, usize), String> {
    for block in doc.blocks.values() {
        if let Some(index) = block.children.iter().position(|c| c == after) {
            return Ok((block.id.clone(), index + 1));
        }
    }
    Err(format!("after 块不存在：{after}"))
}

/// 块树 → HTML 渲染（文本转义 + 白名单标签 + sanitize_html 兜底）。
fn block_to_html(doc: &NoteDoc) -> String {
    fn esc(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }
    fn render(doc: &NoteDoc, id: &str, out: &mut String) {
        let Some(block) = doc.blocks.get(id) else {
            return;
        };
        match &block.kind {
            BlockKind::Paragraph { text } => out.push_str(&format!("<p>{}</p>\n", esc(text))),
            BlockKind::Heading { level, text } => {
                out.push_str(&format!("<h{level}>{}</h{level}>\n", esc(text)))
            }
            BlockKind::List { ordered } => {
                let tag = if *ordered { "ol" } else { "ul" };
                out.push_str(&format!("<{tag}>\n"));
                for child in &block.children {
                    if let Some(item) = doc.blocks.get(child) {
                        if let BlockKind::ListItem { text } = &item.kind {
                            out.push_str(&format!("<li>{}</li>\n", esc(text)));
                        }
                    }
                }
                out.push_str(&format!("</{tag}>\n"));
            }
            BlockKind::ListItem { .. } => {}
            BlockKind::Code { language, text } => {
                out.push_str(&format!(
                    "<pre><code data-lang=\"{}\">{}</code></pre>\n",
                    esc(language),
                    esc(text)
                ));
            }
            BlockKind::Table { rows } => {
                out.push_str("<table>\n");
                for (i, row) in rows.iter().enumerate() {
                    out.push_str("<tr>");
                    for cell in row {
                        if i == 0 {
                            out.push_str(&format!("<th>{}</th>", esc(cell)));
                        } else {
                            out.push_str(&format!("<td>{}</td>", esc(cell)));
                        }
                    }
                    out.push_str("</tr>\n");
                }
                out.push_str("</table>\n");
            }
            BlockKind::Image { src, alt } => {
                out.push_str(&format!(
                    "<img src=\"{}\" alt=\"{}\">\n",
                    esc(src),
                    esc(alt)
                ));
            }
            BlockKind::File { path, .. } => {
                out.push_str(&format!(
                    "<p><a href=\"{}\">📎 {}</a></p>\n",
                    esc(path),
                    esc(path)
                ));
            }
            BlockKind::Quote { text } => {
                out.push_str(&format!("<blockquote>{}</blockquote>\n", esc(text)))
            }
            BlockKind::HtmlEmbed { html } => out.push_str(html),
            BlockKind::Canvas { data } => {
                out.push_str("<div class=\"owo-canvas\">");
                for note in &data.notes {
                    out.push_str(&format!(
                        "<span class=\"owo-canvas-note\">{}</span> ",
                        esc(&note.text)
                    ));
                }
                out.push_str("</div>\n");
            }
            BlockKind::AiGenerated { text, .. } => {
                out.push_str(&format!("<p class=\"owo-ai\">{}</p>\n", esc(text)))
            }
        }
        for child in &block.children {
            render(doc, child, out);
        }
    }
    let mut out = String::new();
    render(doc, &doc.root, &mut out);
    sanitize_html(&out)
}

/// 审计写操作。
fn audit(state: &owo_agent_server::AppState, event: &str, id: &str, detail: impl Into<String>) {
    if let Ok(mut log) = state.agent.audit_log().lock() {
        log.record(
            "notes",
            event,
            Some(id.to_string()),
            Some(true),
            detail.into(),
        );
    }
}

fn err(status: StatusCode, message: impl Into<String>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": message.into() })))
}

// ----------------------------------------------------------------------------
// 路由
// ----------------------------------------------------------------------------

pub fn router(state: Arc<owo_agent_server::AppState>) -> axum::Router {
    axum::Router::new()
        .route("/notes", get(list_notes).post(create_note))
        .route(
            "/notes/{id}",
            get(get_note).put(replace_note).delete(delete_note),
        )
        .route("/notes/import", post(import_note))
        .route("/notes/search", get(search_notes_handler))
        .route("/notes/{id}/export/{format}", get(export_note))
        .route("/notes/{id}/blocks", post(add_block_handler))
        .route("/notes/{id}/blocks/move", post(move_block_handler))
        .route(
            "/notes/{id}/blocks/{block_id}",
            patch(update_block).delete(delete_block),
        )
        .route("/notes/{id}/reindex", post(reindex_note))
        .with_state(state)
}

async fn list_notes(
    State(state): State<Arc<owo_agent_server::AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let store = store_for(&state.data_root);
    let mut store = store.lock().await;
    let list = store
        .list()
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(json!({ "count": list.len(), "notes": list })))
}

async fn create_note(
    State(state): State<Arc<owo_agent_server::AppState>>,
    Json(request): Json<CreateNoteRequest>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let title = request.title.trim().to_string();
    if title.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "title 不能为空"));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let doc = match &request.markdown {
        Some(md) => md_to_doc(id.clone(), title, md),
        None => {
            let mut doc = new_doc(id.clone(), title);
            let root = doc.root.clone();
            let _ = add_block(
                &mut doc,
                &root,
                BlockKind::Paragraph {
                    text: String::new(),
                },
                Default::default(),
            );
            doc
        }
    };
    let store = store_for(&state.data_root);
    let mut store = store.lock().await;
    store
        .persist(&doc)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    audit(
        &state,
        "notes.create",
        &doc.id,
        format!("创建笔记「{}」", doc.title),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({ "ok": true, "id": doc.id, "title": doc.title })),
    ))
}

async fn get_note(
    State(state): State<Arc<owo_agent_server::AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let store = store_for(&state.data_root);
    let store = store.lock().await;
    let doc = store.load(&id).map_err(|e| err(StatusCode::NOT_FOUND, e))?;
    Ok(Json(json!({
        "id": doc.id,
        "title": doc.title,
        "root": doc.root,
        "updated_at": doc.updated_at,
        "blocks": doc.blocks,
    })))
}

async fn replace_note(
    State(state): State<Arc<owo_agent_server::AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<ReplaceNoteRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let store = store_for(&state.data_root);
    let mut store = store.lock().await;
    let mut doc = store.load(&id).map_err(|e| err(StatusCode::NOT_FOUND, e))?;
    if let Some(title) = &request.title {
        if title.trim().is_empty() {
            return Err(err(StatusCode::BAD_REQUEST, "title 不能为空"));
        }
        doc_title(&mut doc, title.clone());
    }
    if let Some(blocks) = &request.blocks {
        let blocks_map: Result<BTreeMap<String, Block>, String> = blocks
            .iter()
            .map(|(key, value)| {
                serde_json::from_value::<Block>(value.clone())
                    .map(|b| (key.clone(), b))
                    .map_err(|e| format!("块 {key} 解析失败：{e}"))
            })
            .collect();
        let blocks_map = blocks_map.map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
        doc.blocks = blocks_map;
        validate_doc(&doc).map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    }
    store
        .persist(&doc)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    audit(&state, "notes.update", &doc.id, "整文档替换");
    Ok(Json(
        json!({ "ok": true, "id": doc.id, "updated_at": doc.updated_at }),
    ))
}

async fn delete_note(
    State(state): State<Arc<owo_agent_server::AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let store = store_for(&state.data_root);
    let mut store = store.lock().await;
    store
        .delete(&id)
        .map_err(|e| err(StatusCode::NOT_FOUND, e))?;
    audit(&state, "notes.delete", &id, "删除笔记");
    Ok(Json(json!({ "ok": true, "id": id })))
}

async fn import_note(
    State(state): State<Arc<owo_agent_server::AppState>>,
    Json(request): Json<ImportRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let id = uuid::Uuid::new_v4().to_string();
    let doc = md_to_doc(id.clone(), request.title, &request.markdown);
    let store = store_for(&state.data_root);
    let mut store = store.lock().await;
    store
        .persist(&doc)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    audit(
        &state,
        "notes.import",
        &doc.id,
        format!("导入 Markdown（{} 块）", doc.blocks.len()),
    );
    Ok(Json(
        json!({ "ok": true, "id": doc.id, "blocks": doc.blocks.len() }),
    ))
}

async fn search_notes_handler(
    State(state): State<Arc<owo_agent_server::AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let q = params.get("q").cloned().unwrap_or_default();
    if q.trim().is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "缺少 q 参数"));
    }
    let store = store_for(&state.data_root);
    let mut store = store.lock().await;
    let hits = store
        .search(&q)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let hits_json: Vec<Value> = hits
        .iter()
        .map(|h| json!({ "doc_id": h.doc_id, "block_id": h.block_id, "snippet": h.snippet }))
        .collect();
    Ok(Json(
        json!({ "q": q, "count": hits.len(), "hits": hits_json }),
    ))
}

async fn export_note(
    State(state): State<Arc<owo_agent_server::AppState>>,
    AxumPath((id, format)): AxumPath<(String, String)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let store = store_for(&state.data_root);
    let store = store.lock().await;
    let doc = store.load(&id).map_err(|e| err(StatusCode::NOT_FOUND, e))?;
    match format.as_str() {
        "md" => Ok(Json(json!({ "format": "md", "content": doc_to_md(&doc) }))),
        "html" => Ok(Json(
            json!({ "format": "html", "content": block_to_html(&doc) }),
        )),
        other => Err(err(
            StatusCode::BAD_REQUEST,
            format!("未知导出格式：{other}（支持 md|html）"),
        )),
    }
}

async fn add_block_handler(
    State(state): State<Arc<owo_agent_server::AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<AddBlockRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let data = request.data.unwrap_or(Value::Null);
    let kind = parse_block_kind(&request.kind, request.text, &data)
        .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    let store = store_for(&state.data_root);
    let mut store = store.lock().await;
    let mut doc = store.load(&id).map_err(|e| err(StatusCode::NOT_FOUND, e))?;
    let (parent, index) = match &request.after {
        Some(after) => locate_after(&doc, after).map_err(|e| err(StatusCode::BAD_REQUEST, e))?,
        None => (
            request.parent.unwrap_or_else(|| doc.root.clone()),
            usize::MAX,
        ),
    };
    if !doc.blocks.contains_key(&parent) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("父块不存在：{parent}"),
        ));
    }
    let new_id = if index == usize::MAX {
        add_block(&mut doc, &parent, kind, Default::default())
            .map_err(|e| err(StatusCode::BAD_REQUEST, e))?
    } else {
        insert_child(&mut doc, &parent, index, kind, Default::default())
            .map_err(|e| err(StatusCode::BAD_REQUEST, e))?
    };
    store
        .persist(&doc)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    audit(
        &state,
        "notes.block.add",
        &id,
        format!("添加块 {new_id}（{}）", request.kind),
    );
    Ok(Json(json!({ "ok": true, "id": new_id })))
}

async fn move_block_handler(
    State(state): State<Arc<owo_agent_server::AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<MoveBlockRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let store = store_for(&state.data_root);
    let mut store = store.lock().await;
    let mut doc = store.load(&id).map_err(|e| err(StatusCode::NOT_FOUND, e))?;
    if !doc.blocks.contains_key(&request.block_id) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("块不存在：{}", request.block_id),
        ));
    }
    let (parent, index) = match &request.after {
        Some(after) => locate_after(&doc, after).map_err(|e| err(StatusCode::BAD_REQUEST, e))?,
        None => (
            request.parent.unwrap_or_else(|| doc.root.clone()),
            usize::MAX,
        ),
    };
    move_block(
        &mut doc,
        &request.block_id,
        &parent,
        if index == usize::MAX {
            None
        } else {
            Some(index)
        },
    )
    .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    store
        .persist(&doc)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    audit(
        &state,
        "notes.block.move",
        &id,
        format!("移动块 {}", request.block_id),
    );
    Ok(Json(json!({ "ok": true, "block_id": request.block_id })))
}

async fn update_block(
    State(state): State<Arc<owo_agent_server::AppState>>,
    AxumPath((id, block_id)): AxumPath<(String, String)>,
    Json(request): Json<UpdateBlockRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let store = store_for(&state.data_root);
    let mut store = store.lock().await;
    let mut doc = store.load(&id).map_err(|e| err(StatusCode::NOT_FOUND, e))?;
    let Some(block) = doc.blocks.get_mut(&block_id) else {
        return Err(err(StatusCode::NOT_FOUND, format!("块不存在：{block_id}")));
    };
    update_block_kind(block, &request).map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    store
        .persist(&doc)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    audit(
        &state,
        "notes.block.update",
        &id,
        format!("更新块 {block_id}"),
    );
    Ok(Json(json!({ "ok": true, "block_id": block_id })))
}

fn update_block_kind(block: &mut Block, request: &UpdateBlockRequest) -> Result<(), String> {
    let text = request.text.clone().unwrap_or_default();
    match &mut block.kind {
        BlockKind::Paragraph { text: t } => {
            if request.text.is_some() {
                *t = text;
            }
        }
        BlockKind::Heading { text: t, .. } => {
            if request.text.is_some() {
                *t = text;
            }
        }
        BlockKind::ListItem { text: t } => {
            if request.text.is_some() {
                *t = text;
            }
        }
        BlockKind::Code { text: t, .. } => {
            if request.text.is_some() {
                *t = text;
            }
        }
        BlockKind::Quote { text: t } => {
            if request.text.is_some() {
                *t = text;
            }
        }
        BlockKind::AiGenerated { text: t, .. } => {
            if request.text.is_some() {
                *t = text;
            }
        }
        BlockKind::Table { rows } => {
            if let Some(data) = &request.data {
                if let Some(new_rows) = data.get("rows").and_then(Value::as_array) {
                    *rows = new_rows
                        .iter()
                        .filter_map(Value::as_array)
                        .map(|row| {
                            row.iter()
                                .map(|c| c.as_str().unwrap_or("").to_string())
                                .collect()
                        })
                        .collect();
                }
            }
        }
        BlockKind::Image { src, .. } => {
            if let Some(data) = &request.data {
                if let Some(new_src) = data.get("src").and_then(Value::as_str) {
                    *src = new_src.to_string();
                }
            }
        }
        BlockKind::File { path, .. } => {
            if let Some(data) = &request.data {
                if let Some(new_path) = data.get("path").and_then(Value::as_str) {
                    *path = new_path.to_string();
                }
            }
        }
        BlockKind::HtmlEmbed { html } => {
            if request.text.is_some() {
                *html = sanitize_html(&text);
            }
        }
        BlockKind::Canvas { data } => {
            if let Some(new_data) = &request.data {
                *data = parse_canvas(new_data);
            }
        }
        BlockKind::List { .. } => {}
    }
    Ok(())
}

async fn delete_block(
    State(state): State<Arc<owo_agent_server::AppState>>,
    AxumPath((id, block_id)): AxumPath<(String, String)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let store = store_for(&state.data_root);
    let mut store = store.lock().await;
    let mut doc = store.load(&id).map_err(|e| err(StatusCode::NOT_FOUND, e))?;
    if !doc.blocks.contains_key(&block_id) {
        return Err(err(StatusCode::NOT_FOUND, format!("块不存在：{block_id}")));
    }
    let removed = remove_block(&mut doc, &block_id).map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    store
        .persist(&doc)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    audit(
        &state,
        "notes.block.delete",
        &id,
        format!("删除块 {block_id}（子树 {} 块）", removed.len()),
    );
    Ok(Json(json!({ "ok": true, "removed": removed })))
}

async fn reindex_note(
    State(state): State<Arc<owo_agent_server::AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let store = store_for(&state.data_root);
    let mut store = store.lock().await;
    let doc = store.load(&id).map_err(|e| err(StatusCode::NOT_FOUND, e))?;
    store
        .reindex(&doc)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(json!({ "ok": true, "id": id, "reindexed": true })))
}

// 测试辅助：块文本（供导出与搜索调试）。
#[allow(dead_code)]
fn block_text_debug(doc: &NoteDoc, id: &BlockId) -> String {
    doc.blocks
        .get(id)
        .map(|b| block_text(doc, b))
        .unwrap_or_default()
}
