//! M4c 多格式文档模型 v1（技术文档 §5.6）
//!
//! 文档 = 块树：Markdown / HTML 嵌入 / 画布 是同一模型的渲染器，不是互转格式。
//! - 块有稳定 id、属性（attrs）、有序子块；操作全部为纯函数（&mut NoteDoc），便于单测。
//! - 持久化：`<dir>/doc.json` + `<dir>/assets/` 资源目录；读写往返无损。
//! - Markdown 导入/导出常用元素往返无损；HTML 嵌入块保留原始片段但剥离
//!   脚本/事件属性/危险 URL（安全契约）。
//! - 全文检索：内存分词索引 + SQLite FTS5（rusqlite bundled，零新增依赖）。
//!
//! 安全基线：HtmlEmbed 内容入库前必须经 `sanitize_html` 消毒，禁止任何可执行内容。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// 块 id（文档内唯一）。
pub type BlockId = String;

/// 文档对象：块树 + 元数据。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NoteDoc {
    pub id: String,
    pub title: String,
    /// 根块 id（唯一根，通常为隐含容器块）。
    pub root: BlockId,
    pub blocks: BTreeMap<BlockId, Block>,
    pub updated_at: String,
}

/// 块：稳定 id + 类型 + 通用属性 + 有序子块。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Block {
    pub id: BlockId,
    pub kind: BlockKind,
    /// 通用属性（扩展字段；画布数据/表格/代码语言等结构化字段见 BlockKind）。
    #[serde(default)]
    pub attrs: BTreeMap<String, serde_json::Value>,
    pub children: Vec<BlockId>,
}

/// 块类型：覆盖 段落/标题/列表/代码/表格/图片/文件/引用/HTML嵌入/画布/AI生成。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum BlockKind {
    Paragraph {
        text: String,
    },
    Heading {
        level: u8,
        text: String,
    },
    List {
        ordered: bool,
    },
    ListItem {
        text: String,
    },
    Code {
        language: String,
        text: String,
    },
    Table {
        rows: Vec<Vec<String>>,
    },
    Image {
        src: String,
        alt: String,
    },
    File {
        path: String,
        mime: String,
    },
    Quote {
        text: String,
    },
    /// 已消毒的 HTML 片段（禁止脚本/事件属性/危险 URL）。
    HtmlEmbed {
        html: String,
    },
    /// 画布：矩形/层/便签文本（数据往返保证，渲染留给前端）。
    Canvas {
        data: CanvasBlockData,
    },
    AiGenerated {
        model: String,
        prompt: String,
        text: String,
    },
}

/// 画布块最小数据模型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CanvasBlockData {
    pub rects: Vec<CanvasRect>,
    pub notes: Vec<CanvasNote>,
    /// 层名（按 z 序，先画先垫底）。
    pub layers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CanvasRect {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// 所属层（缺省 ""）。
    #[serde(default)]
    pub layer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CanvasNote {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub text: String,
}

// ----------------------------------------------------------------------------
// 块树操作（纯函数）
// ----------------------------------------------------------------------------

/// 新建空文档（含隐含根块）。
pub fn new_doc(id: impl Into<String>, title: impl Into<String>) -> NoteDoc {
    let root = BlockId::from("root");
    let mut blocks = BTreeMap::new();
    blocks.insert(
        root.clone(),
        Block {
            id: root.clone(),
            kind: BlockKind::Paragraph {
                text: String::new(),
            },
            attrs: BTreeMap::new(),
            children: Vec::new(),
        },
    );
    NoteDoc {
        id: id.into(),
        title: title.into(),
        root,
        blocks,
        updated_at: now_rfc3339(),
    }
}

/// 生成唯一块 id。
fn fresh_id(doc: &NoteDoc) -> BlockId {
    let mut n = doc.blocks.len();
    loop {
        let candidate = format!("b{n:06}");
        if !doc.blocks.contains_key(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn touch(doc: &mut NoteDoc) {
    doc.updated_at = now_rfc3339();
}

/// 在 parent 下追加子块，返回新块 id。
pub fn add_block(
    doc: &mut NoteDoc,
    parent: &BlockId,
    kind: BlockKind,
    attrs: BTreeMap<String, serde_json::Value>,
) -> Result<BlockId, String> {
    if !doc.blocks.contains_key(parent) {
        return Err(format!("父块不存在：{parent}"));
    }
    let id = fresh_id(doc);
    doc.blocks.insert(
        id.clone(),
        Block {
            id: id.clone(),
            kind,
            attrs,
            children: Vec::new(),
        },
    );
    doc.blocks
        .get_mut(parent)
        .expect("已校验存在")
        .children
        .push(id.clone());
    touch(doc);
    Ok(id)
}

/// 在 parent 的 index 位置插入子块。
pub fn insert_child(
    doc: &mut NoteDoc,
    parent: &BlockId,
    index: usize,
    kind: BlockKind,
    attrs: BTreeMap<String, serde_json::Value>,
) -> Result<BlockId, String> {
    if !doc.blocks.contains_key(parent) {
        return Err(format!("父块不存在：{parent}"));
    }
    let id = fresh_id(doc);
    doc.blocks.insert(
        id.clone(),
        Block {
            id: id.clone(),
            kind,
            attrs,
            children: Vec::new(),
        },
    );
    let children = &mut doc.blocks.get_mut(parent).expect("已校验存在").children;
    let index = index.min(children.len());
    children.insert(index, id.clone());
    touch(doc);
    Ok(id)
}

/// 删除块及其子树（返回被删块 id 集合）。
pub fn remove_block(doc: &mut NoteDoc, id: &BlockId) -> Result<Vec<BlockId>, String> {
    if id == &doc.root {
        return Err("不能删除根块".to_string());
    }
    if !doc.blocks.contains_key(id) {
        return Err(format!("块不存在：{id}"));
    }
    // 从父块 children 摘除
    let parent_id = doc
        .blocks
        .values()
        .find(|b| b.children.contains(id))
        .map(|b| b.id.clone());
    if let Some(parent_id) = parent_id {
        if let Some(parent) = doc.blocks.get_mut(&parent_id) {
            parent.children.retain(|child| child != id);
        }
    }
    // 递归收集子树
    let mut removed = Vec::new();
    let mut stack = vec![id.clone()];
    while let Some(current) = stack.pop() {
        if let Some(block) = doc.blocks.remove(&current) {
            removed.push(current.clone());
            stack.extend(block.children);
        }
    }
    touch(doc);
    Ok(removed)
}

/// 移动块到新父（可选指定位置）；校验不成环（不能移进自己子树）。
pub fn move_block(
    doc: &mut NoteDoc,
    id: &BlockId,
    new_parent: &BlockId,
    index: Option<usize>,
) -> Result<(), String> {
    if id == &doc.root {
        return Err("不能移动根块".to_string());
    }
    if id == new_parent {
        return Err("不能把块移入自身".to_string());
    }
    if !doc.blocks.contains_key(id) || !doc.blocks.contains_key(new_parent) {
        return Err("块不存在".to_string());
    }
    // 环检测：new_parent 不得位于 id 的子树内
    let mut cursor = new_parent.clone();
    while cursor != doc.root {
        let parent_of = doc
            .blocks
            .values()
            .find(|b| b.children.contains(&cursor))
            .map(|b| b.id.clone());
        match parent_of {
            Some(up) if up == *id => return Err("移动会造成环：目标在自身子树内".to_string()),
            Some(up) => cursor = up,
            None => break,
        }
    }
    // 从原父摘除
    let old_parent = doc
        .blocks
        .values()
        .find(|b| b.children.contains(id))
        .map(|b| b.id.clone());
    if let Some(old_parent) = old_parent {
        if let Some(parent) = doc.blocks.get_mut(&old_parent) {
            parent.children.retain(|child| child != id);
        }
    }
    let children = &mut doc.blocks.get_mut(new_parent).expect("已校验存在").children;
    let index = index.unwrap_or(children.len()).min(children.len());
    children.insert(index, id.clone());
    touch(doc);
    Ok(())
}

/// 读取块（克隆）。
pub fn get_block(doc: &NoteDoc, id: &BlockId) -> Option<Block> {
    doc.blocks.get(id).cloned()
}

/// 设置文档标题。
pub fn doc_title(doc: &mut NoteDoc, title: impl Into<String>) {
    doc.title = title.into();
    touch(doc);
}

/// 追加子块（add_block 的便捷封装，返回 id）。
pub fn append_child(
    doc: &mut NoteDoc,
    parent: &BlockId,
    kind: BlockKind,
) -> Result<BlockId, String> {
    add_block(doc, parent, kind, BTreeMap::new())
}

/// 遍历子树（含自身），用于索引/导出。
pub fn walk<'a>(doc: &'a NoteDoc, root: &BlockId) -> Vec<&'a Block> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if let Some(block) = doc.blocks.get(id) {
            out.push(block);
            stack.extend(block.children.iter().rev());
        }
    }
    out
}

// ----------------------------------------------------------------------------
// 持久化：<dir>/doc.json + <dir>/assets/
// ----------------------------------------------------------------------------

/// 保存文档到 `<dir>/doc.json`（原子写：先写临时文件再改名）。
pub fn save_doc(doc: &NoteDoc, dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败：{e}"))?;
    std::fs::create_dir_all(dir.join("assets")).map_err(|e| format!("创建资源目录失败：{e}"))?;
    let content = serde_json::to_string_pretty(doc).map_err(|e| e.to_string())?;
    let target = dir.join("doc.json");
    let tmp = dir.join("doc.json.tmp");
    std::fs::write(&tmp, content).map_err(|e| format!("写入失败：{e}"))?;
    std::fs::rename(&tmp, &target).map_err(|e| format!("替换失败：{e}"))
}

/// 从 `<dir>/doc.json` 加载。
pub fn load_doc(dir: &Path) -> Result<NoteDoc, String> {
    let content =
        std::fs::read_to_string(dir.join("doc.json")).map_err(|e| format!("读取失败：{e}"))?;
    serde_json::from_str(&content).map_err(|e| format!("解析失败：{e}"))
}

// ----------------------------------------------------------------------------
// HTML 消毒（安全契约：无脚本/无事件属性/无危险 URL）
// ----------------------------------------------------------------------------

/// 允许的标签白名单。
const HTML_TAGS: &[&str] = &[
    "p",
    "div",
    "span",
    "strong",
    "em",
    "b",
    "i",
    "u",
    "s",
    "sub",
    "sup",
    "ul",
    "ol",
    "li",
    "table",
    "thead",
    "tbody",
    "tr",
    "th",
    "td",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "code",
    "pre",
    "blockquote",
    "a",
    "img",
    "br",
    "hr",
    "mark",
    "small",
    "del",
];

/// 永久移除的标签（含内容）。
const HTML_BLOCKED_TAGS: &[&str] = &[
    "script", "style", "iframe", "object", "embed", "form", "input", "button", "textarea",
    "select", "link", "meta", "base", "svg", "math", "video", "audio", "canvas", "template",
    "noscript",
];

/// 允许的 URL 协议（href/src）。
fn safe_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_lowercase();
    if lower.starts_with("javascript:")
        || lower.starts_with("data:")
        || lower.starts_with("vbscript:")
        || lower.contains('<')
        || lower.contains('>')
    {
        return None;
    }
    // 协议白名单：http/https/mailto/tel/# 或相对路径
    if let Some(colon) = lower.find(':') {
        let scheme = &lower[..colon];
        if !matches!(scheme, "http" | "https" | "mailto" | "tel") {
            return None;
        }
    }
    Some(trimmed.to_string())
}

/// 允许的属性白名单（按标签泛化）。
fn safe_attrs(tag: &str, attrs: &[(String, String)]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (name, value) in attrs {
        let lower = name.to_lowercase();
        if lower.starts_with("on") {
            continue; // 事件属性一律剥离
        }
        match lower.as_str() {
            "href" | "src" => {
                if let Some(url) = safe_url(value) {
                    out.push((name.clone(), url));
                }
            }
            "alt" | "title" | "lang" | "dir" | "colspan" | "rowspan" | "width" | "height"
            | "class" | "id" | "align" | "start" => {
                out.push((name.clone(), value.clone()));
            }
            _ => {} // 其余（style/on* 等）剥离
        }
    }
    let _ = tag;
    out
}

/// 消毒 HTML：剥离危险标签（含内容）与事件属性，URL 白名单校验。
/// 保留原始片段的可见结构（标签/文本/属性中的安全子集）。
pub fn sanitize_html(raw: &str) -> String {
    // 先整体移除被禁标签（含内容，大小写不敏感，非贪婪）。
    let mut text = raw.to_string();
    for tag in HTML_BLOCKED_TAGS {
        let mut lower = text.to_lowercase();
        while let Some(start) = lower.find(&format!("<{tag}")) {
            // 找结束位置：先找闭合标签，找不到则到标签结束（自闭合）或文本末尾
            let open_end = text[start..]
                .find('>')
                .map(|i| start + i + 1)
                .unwrap_or(text.len());
            let close_tag = format!("</{tag}>");
            let close_end = lower[open_end..]
                .find(&close_tag)
                .map(|i| open_end + i + close_tag.len());
            match close_end {
                Some(end) => {
                    text.replace_range(start..end, "");
                }
                None => {
                    // 无闭合：若标签自闭合（/>）则只删标签本身，否则删到末尾
                    if text[open_end.saturating_sub(2)..open_end.min(text.len())].contains("/>") {
                        text.replace_range(start..open_end, "");
                    } else {
                        text.replace_range(start..text.len(), "");
                    }
                }
            }
            lower = text.to_lowercase();
        }
    }
    // 逐标签清洗：白名单外的标签去掉尖括号但保留内容（转义文本），白名单内清洗属性。
    let mut out = String::new();
    let mut rest = text.as_str();
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        rest = &rest[lt..];
        let Some(gt) = rest.find('>') else {
            // 未闭合的 < → 转义
            out.push_str("&lt;");
            rest = &rest[1..];
            continue;
        };
        let token = &rest[1..gt];
        rest = &rest[gt + 1..];
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let (is_close, token) = if let Some(rest) = token.strip_prefix('/') {
            (true, rest.trim())
        } else {
            (false, token)
        };
        let mut parts = token.splitn(2, char::is_whitespace);
        let tag = parts.next().unwrap_or("").to_lowercase();
        let attr_text = parts.next().unwrap_or("");
        if !HTML_TAGS.contains(&tag.as_str()) {
            // 未知标签：保留为转义文本（不吞内容）
            out.push_str("&lt;");
            out.push_str(escape_html(token).as_str());
            out.push_str("&gt;");
            continue;
        }
        // 解析属性
        let attrs = parse_attrs(attr_text);
        let clean = safe_attrs(&tag, &attrs);
        let mut rendered = format!("<{}", if is_close { "/" } else { "" });
        rendered.push_str(&tag);
        for (name, value) in clean {
            rendered.push_str(&format!(" {}=\"{}\"", name, escape_attr(&value)));
        }
        rendered.push('>');
        out.push_str(&rendered);
    }
    out.push_str(rest);
    out
}

fn parse_attrs(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.trim().is_empty() {
        rest = rest.trim_start();
        let name_end = rest
            .find(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or(rest.len());
        let name = rest[..name_end].to_string();
        if name.is_empty() {
            break;
        }
        rest = &rest[name_end..];
        let mut value = String::new();
        if let Some(eq) = rest.find('=') {
            if eq == 0 {
                rest = rest[1..].trim_start();
                if let Some(quote) = rest.chars().next() {
                    if quote == '"' || quote == '\'' {
                        rest = &rest[1..];
                        let end = rest.find(quote).unwrap_or(rest.len());
                        value = rest[..end].to_string();
                        rest = &rest[end.min(rest.len())..];
                        if end < rest.len() + 1 && rest.starts_with(quote) {
                            rest = &rest[1..];
                        }
                    } else {
                        let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
                        value = rest[..end].to_string();
                        rest = &rest[end..];
                    }
                }
            }
        }
        out.push((name, value));
    }
    out
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(text: &str) -> String {
    escape_html(text).replace('"', "&quot;")
}

// ----------------------------------------------------------------------------
// Markdown 导入/导出（常用元素往返无损）
// ----------------------------------------------------------------------------

/// Markdown 文本 → 块树文档（按 root 下顺序块组织；列表项挂到 List 下）。
pub fn md_to_doc(id: impl Into<String>, title: impl Into<String>, md: &str) -> NoteDoc {
    let mut doc = new_doc(id, title);
    let root = doc.root.clone();
    let mut stack: Vec<BlockId> = vec![root.clone()]; // 栈顶为当前容器（root/List/Quote）
    let mut pending_list: Option<BlockId> = None; // 当前列表容器（连续列表项归组）
    let mut pending_list_ordered = false;

    let lines: Vec<&str> = md.split('\n').collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_end();
        let leading = line.len() - line.trim_start().len();
        let _ = leading;

        // 代码块（``` 围栏）
        if let Some(language) = trimmed.strip_prefix("```") {
            let language = language.trim().to_string();
            let mut code_lines = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim().starts_with("```") {
                code_lines.push(lines[i]);
                i += 1;
            }
            i += 1; // 跳过闭合围栏
            pending_list = None;
            let _ = add_block(
                &mut doc,
                &root,
                BlockKind::Code {
                    language,
                    text: code_lines.join("\n"),
                },
                BTreeMap::new(),
            );
            continue;
        }

        // 标题
        if let Some(level) = heading_level(trimmed) {
            pending_list = None;
            let _ = add_block(
                &mut doc,
                &root,
                BlockKind::Heading {
                    level,
                    text: trimmed[level as usize + 1..].trim().to_string(),
                },
                BTreeMap::new(),
            );
            i += 1;
            continue;
        }

        // 引用
        if trimmed.starts_with("> ") || trimmed == ">" {
            pending_list = None;
            let quote = trimmed.trim_start_matches('>').trim().to_string();
            let _ = add_block(
                &mut doc,
                &root,
                BlockKind::Quote { text: quote },
                BTreeMap::new(),
            );
            i += 1;
            continue;
        }

        // 列表项
        if let Some((ordered, text)) = list_item(trimmed) {
            if pending_list.is_none()
                || pending_list_ordered != ordered
                || !doc.blocks.get(pending_list.as_ref().unwrap()).is_some_and(
                    |b| matches!(b.kind, BlockKind::List { ordered: o } if o == ordered),
                )
            {
                pending_list = Some(
                    add_block(
                        &mut doc,
                        &root,
                        BlockKind::List { ordered },
                        BTreeMap::new(),
                    )
                    .expect("append list"),
                );
                pending_list_ordered = ordered;
            }
            let list_id = pending_list.clone().unwrap();
            let _ = add_block(
                &mut doc,
                &list_id,
                BlockKind::ListItem { text },
                BTreeMap::new(),
            );
            i += 1;
            continue;
        }
        pending_list = None;

        // 表格（| a | b | 且下一行为分隔行 |---|）
        if trimmed.starts_with('|')
            && i + 1 < lines.len()
            && is_table_separator(lines[i + 1].trim())
        {
            let header: Vec<String> = trimmed
                .trim_matches('|')
                .split('|')
                .map(|c| c.trim().to_string())
                .collect();
            let mut rows = vec![header];
            i += 2; // 跳过表头 + 分隔行
            while i < lines.len() && lines[i].trim().starts_with('|') {
                rows.push(
                    lines[i]
                        .trim()
                        .trim_matches('|')
                        .split('|')
                        .map(|c| c.trim().to_string())
                        .collect(),
                );
                i += 1;
            }
            let _ = add_block(&mut doc, &root, BlockKind::Table { rows }, BTreeMap::new());
            continue;
        }

        // 图片
        if let Some((alt, src)) = image_line(trimmed) {
            let _ = add_block(
                &mut doc,
                &root,
                BlockKind::Image { src, alt },
                BTreeMap::new(),
            );
            i += 1;
            continue;
        }

        // HTML 嵌入（以 < 开头的行，经消毒入库）
        if trimmed.starts_with('<') {
            let _ = add_block(
                &mut doc,
                &root,
                BlockKind::HtmlEmbed {
                    html: sanitize_html(trimmed),
                },
                BTreeMap::new(),
            );
            i += 1;
            continue;
        }

        // 段落（空行忽略）
        if !trimmed.is_empty() {
            let _ = add_block(
                &mut doc,
                &root,
                BlockKind::Paragraph {
                    text: trimmed.to_string(),
                },
                BTreeMap::new(),
            );
        }
        i += 1;
    }
    let _ = &mut stack;
    doc
}

fn heading_level(line: &str) -> Option<u8> {
    let trimmed = line.trim_start();
    let level = trimmed.bytes().take_while(|b| *b == b'#').count();
    if (1..=6).contains(&level) && trimmed.as_bytes().get(level) == Some(&b' ') {
        Some(level as u8)
    } else {
        None
    }
}

fn list_item(line: &str) -> Option<(bool, String)> {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
    {
        return Some((false, rest.trim().to_string()));
    }
    let digits: usize = trimmed.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits > 0 && trimmed.as_bytes().get(digits) == Some(&b'.') {
        let rest = &trimmed[digits + 1..];
        if let Some(text) = rest.strip_prefix(' ') {
            return Some((true, text.trim().to_string()));
        }
    }
    None
}

fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|') && trimmed.contains('-') && trimmed.matches('-').count() >= 2
}

fn image_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if !trimmed.starts_with("![") {
        return None;
    }
    let close = trimmed.find("](")?;
    let alt = trimmed[2..close].to_string();
    let rest = &trimmed[close + 2..];
    let end = rest.find(')')?;
    let src = rest[..end].to_string();
    Some((alt, src))
}

/// 块树文档 → Markdown 文本。
pub fn doc_to_md(doc: &NoteDoc) -> String {
    let root = doc.root.clone();
    let mut out = String::new();
    render_children_to_md(doc, &root, &mut out);
    out
}

fn render_children_to_md(doc: &NoteDoc, parent: &BlockId, out: &mut String) {
    let Some(block) = doc.blocks.get(parent) else {
        return;
    };
    for child_id in &block.children {
        let Some(child) = doc.blocks.get(child_id) else {
            continue;
        };
        match &child.kind {
            BlockKind::Paragraph { text } => {
                out.push_str(text);
                out.push_str("\n\n");
            }
            BlockKind::Heading { level, text } => {
                out.push_str(&"#".repeat(*level as usize));
                out.push(' ');
                out.push_str(text);
                out.push_str("\n\n");
            }
            BlockKind::List { ordered } => {
                for (index, item_id) in child.children.iter().enumerate() {
                    if let Some(item) = doc.blocks.get(item_id) {
                        if let BlockKind::ListItem { text } = &item.kind {
                            let marker = if *ordered {
                                format!("{}. ", index + 1)
                            } else {
                                "- ".to_string()
                            };
                            out.push_str(&marker);
                            out.push_str(text);
                            out.push('\n');
                        }
                    }
                }
                out.push('\n');
            }
            BlockKind::ListItem { .. } => {} // 由 List 统一渲染
            BlockKind::Code { language, text } => {
                out.push_str("```");
                out.push_str(language);
                out.push('\n');
                out.push_str(text);
                out.push_str("\n```\n\n");
            }
            BlockKind::Table { rows } => {
                if let Some((first, rest)) = rows.split_first() {
                    out.push_str("| ");
                    out.push_str(&first.join(" | "));
                    out.push_str(" |\n| ");
                    out.push_str(&vec!["---"; first.len()].join(" | "));
                    out.push_str(" |\n");
                    for row in rest {
                        out.push_str("| ");
                        out.push_str(&row.join(" | "));
                        out.push_str(" |\n");
                    }
                    out.push('\n');
                }
            }
            BlockKind::Image { src, alt } => {
                out.push_str(&format!("![{alt}]({src})\n\n"));
            }
            BlockKind::File { path, .. } => {
                out.push_str(&format!("[📎 {path}]({path})\n\n"));
            }
            BlockKind::Quote { text } => {
                for line in text.lines() {
                    out.push_str("> ");
                    out.push_str(line);
                    out.push('\n');
                }
                out.push('\n');
            }
            BlockKind::HtmlEmbed { html } => {
                out.push_str(html);
                out.push_str("\n\n");
            }
            BlockKind::Canvas { .. } => {
                // 画布数据模型不进入 Markdown 渲染（渲染留给前端）；导出为空段落占位。
                out.push_str("\n\n");
            }
            BlockKind::AiGenerated { .. } => {
                // AI 生成块不进入 Markdown（v1：数据保真由 doc.json 承担，MD 为可表达元素渲染器）。
                out.push_str("\n\n");
            }
        }
        render_children_to_md(doc, child_id, out);
    }
}

// ----------------------------------------------------------------------------
// 全文索引：内存分词 + SQLite FTS5（零新增依赖）
// ----------------------------------------------------------------------------

/// 检索命中。
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub doc_id: String,
    pub block_id: BlockId,
    /// 命中词（FTS 片段或内存命中的原词）。
    pub snippet: String,
}

/// 从块提取可检索/可展示文本（递归含子块；供索引、渲染器、测试使用）。
pub fn block_text(doc: &NoteDoc, block: &Block) -> String {
    let mut text = String::new();
    match &block.kind {
        BlockKind::Paragraph { text: t } => text.push_str(t),
        BlockKind::Heading { text: t, .. } => text.push_str(t),
        BlockKind::ListItem { text: t } => text.push_str(t),
        BlockKind::Code { text: t, .. } => text.push_str(t),
        BlockKind::Table { rows } => {
            for row in rows {
                text.push_str(&row.join(" "));
            }
        }
        BlockKind::Image { alt, .. } => text.push_str(alt),
        BlockKind::File { path, .. } => text.push_str(path),
        BlockKind::Quote { text: t } => text.push_str(t),
        BlockKind::HtmlEmbed { html } => text.push_str(html),
        BlockKind::Canvas { data } => {
            for note in &data.notes {
                text.push_str(&note.text);
            }
        }
        BlockKind::AiGenerated {
            prompt, text: t, ..
        } => {
            text.push_str(prompt);
            text.push(' ');
            text.push_str(t);
        }
        BlockKind::List { .. } => {}
    }
    // 递归子块
    for child_id in &block.children {
        if let Some(child) = doc.blocks.get(child_id) {
            text.push(' ');
            text.push_str(&block_text(doc, child));
        }
    }
    text
}

/// 简单分词：按非字母数字切分 + 小写。
fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            current.push(ch.to_lowercase().next().unwrap_or(ch));
        } else if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// 索引 trait：内存实现与 FTS5 实现共用。
pub trait NoteIndex: Send {
    /// 全量重建文档索引（幂等）。
    fn index_doc(&mut self, doc: &NoteDoc) -> Result<(), String>;
    /// 检索，返回命中的块（按文档内顺序）。
    fn search(&self, query: &str) -> Vec<SearchHit>;
}

/// 内存分词索引（简单、无文件依赖）。
#[derive(Default)]
pub struct InMemoryNoteIndex {
    /// 词 → (doc_id, block_id, snippet)
    map: BTreeMap<String, Vec<(String, String, String)>>,
}

impl InMemoryNoteIndex {
    pub fn new() -> Self {
        Self::default()
    }
}

impl NoteIndex for InMemoryNoteIndex {
    fn index_doc(&mut self, doc: &NoteDoc) -> Result<(), String> {
        self.map.clear();
        for block in walk(doc, &doc.root) {
            let text = block_text(doc, block);
            if text.trim().is_empty() {
                continue;
            }
            let snippet: String = text.chars().take(60).collect();
            for word in tokenize(&text) {
                if word.len() < 2 {
                    continue;
                }
                self.map.entry(word).or_default().push((
                    doc.id.clone(),
                    block.id.clone(),
                    snippet.clone(),
                ));
            }
        }
        Ok(())
    }

    fn search(&self, query: &str) -> Vec<SearchHit> {
        let words: Vec<String> = tokenize(query)
            .into_iter()
            .filter(|w| w.len() >= 2)
            .collect();
        if words.is_empty() {
            return Vec::new();
        }
        let mut hits = Vec::new();
        for (word, entries) in &self.map {
            if words
                .iter()
                .any(|w| word.starts_with(w) || w.starts_with(word))
            {
                for (doc_id, block_id, snippet) in entries {
                    hits.push(SearchHit {
                        doc_id: doc_id.clone(),
                        block_id: block_id.clone(),
                        snippet: snippet.clone(),
                    });
                }
            }
        }
        hits.sort_by(|a, b| a.doc_id.cmp(&b.doc_id).then(a.block_id.cmp(&b.block_id)));
        hits.dedup();
        hits
    }
}

/// SQLite FTS5 索引（`<db_path>` 单文件；重建时清空重插）。
/// tokenizer 用 trigram：对中文/无空格语言的子串检索友好；<3 字符查询回退 LIKE。
pub struct FtsNoteIndex {
    conn: std::sync::Mutex<rusqlite::Connection>,
}

impl FtsNoteIndex {
    pub fn open(db_path: &Path) -> Result<Self, String> {
        let conn = rusqlite::Connection::open(db_path).map_err(|e| e.to_string())?;
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS note_fts USING fts5(doc_id, block_id UNINDEXED, text, tokenize='trigram');",
        )
        .map_err(|e| e.to_string())?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
        })
    }
}

impl NoteIndex for FtsNoteIndex {
    fn index_doc(&mut self, doc: &NoteDoc) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch("DELETE FROM note_fts;")
            .map_err(|e| e.to_string())?;
        for block in walk(doc, &doc.root) {
            let text = block_text(doc, block);
            if text.trim().is_empty() {
                continue;
            }
            conn.execute(
                "INSERT INTO note_fts(doc_id, block_id, text) VALUES (?1, ?2, ?3)",
                rusqlite::params![doc.id, block.id, text],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn search(&self, query: &str) -> Vec<SearchHit> {
        let query = query.trim();
        if query.is_empty() {
            return Vec::new();
        }
        let conn = self.conn.lock().unwrap();
        // 短查询（trigram 要求 ≥3 字符）：LIKE 回退
        let chars: usize = query.chars().count();
        if chars < 3 {
            let like = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
            let mut stmt = match conn.prepare(
                "SELECT doc_id, block_id, substr(text, 1, 60) FROM note_fts WHERE text LIKE ?1 ESCAPE '\\'",
            ) {
                Ok(stmt) => stmt,
                Err(_) => return Vec::new(),
            };
            let rows = stmt.query_map(rusqlite::params![like], |row| {
                Ok(SearchHit {
                    doc_id: row.get(0)?,
                    block_id: row.get(1)?,
                    snippet: row.get(2)?,
                })
            });
            return match rows {
                Ok(rows) => rows.flatten().collect(),
                Err(_) => Vec::new(),
            };
        }
        // 查询词转 FTS 短语（trigram 子串匹配）
        let phrase = format!("\"{}\"", query.replace('"', "\"\""));
        let mut stmt = match conn.prepare(
            "SELECT doc_id, block_id, snippet(note_fts, 2, '[', ']', '...', 12) FROM note_fts WHERE note_fts MATCH ?1",
        ) {
            Ok(stmt) => stmt,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(rusqlite::params![phrase], |row| {
            Ok(SearchHit {
                doc_id: row.get(0)?,
                block_id: row.get(1)?,
                snippet: row.get(2)?,
            })
        });
        match rows {
            Ok(rows) => rows.flatten().collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// 便捷入口：索引器（内存为主，可选 FTS）。
pub struct NoteIndexer {
    inner: Box<dyn NoteIndex>,
}

impl NoteIndexer {
    pub fn in_memory() -> Self {
        Self {
            inner: Box::new(InMemoryNoteIndex::new()),
        }
    }

    pub fn fts(db_path: &Path) -> Result<Self, String> {
        Ok(Self {
            inner: Box::new(FtsNoteIndex::open(db_path)?),
        })
    }

    pub fn index_doc(&mut self, doc: &NoteDoc) -> Result<(), String> {
        self.inner.index_doc(doc)
    }
}

impl NoteIndex for NoteIndexer {
    fn index_doc(&mut self, doc: &NoteDoc) -> Result<(), String> {
        self.inner.index_doc(doc)
    }

    fn search(&self, query: &str) -> Vec<SearchHit> {
        self.inner.search(query)
    }
}

/// 便捷函数：内存索引检索。
pub fn search_notes(index: &NoteIndexer, query: &str) -> Vec<SearchHit> {
    index.search(query)
}

// ----------------------------------------------------------------------------
// 测试辅助：程序化混合文档生成器（100 份样例往返）
// ----------------------------------------------------------------------------

/// 以 seed 确定性生成混合文档：段落/标题/列表/代码/表格/图片/引用/HTML/画布/AI 块。
pub fn generate_mixed_doc(
    seed: u64,
    doc_id: impl Into<String>,
    title: impl Into<String>,
) -> NoteDoc {
    let mut doc = new_doc(doc_id, title);
    let root = doc.root.clone();
    let mut s = seed;
    let mut next = || {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (s >> 33) as u32
    };
    let topics = [
        "项目计划",
        "周报",
        "需求评审",
        "设计文档",
        "会议纪要",
        "发布说明",
        "任务清单",
        "性能优化",
        "用户反馈",
        "架构决策",
    ];
    let topic = topics[(next() as usize) % topics.len()];
    let count = 6 + (next() % 12);
    for _ in 0..count {
        let kind = next() % 11;
        match kind {
            0 => {
                let _ = append_child(
                    &mut doc,
                    &root,
                    BlockKind::Heading {
                        level: 1 + (next() % 3) as u8,
                        text: format!("{topic} 章节-{}", next() % 10),
                    },
                );
            }
            1 => {
                let _ = append_child(
                    &mut doc,
                    &root,
                    BlockKind::Paragraph {
                        text: format!(
                            "{} 的说明文本，包含关键词 {} 与数字 {}",
                            topic,
                            next() % 1000,
                            next() % 100
                        ),
                    },
                );
            }
            2 => {
                let list_id = append_child(
                    &mut doc,
                    &root,
                    BlockKind::List {
                        ordered: next() % 2 == 0,
                    },
                )
                .unwrap();
                for i in 0..(1 + next() % 4) {
                    let _ = append_child(
                        &mut doc,
                        &list_id,
                        BlockKind::ListItem {
                            text: format!("条目 {}：{}", i + 1, topic),
                        },
                    );
                }
            }
            3 => {
                let _ = append_child(
                    &mut doc,
                    &root,
                    BlockKind::Code {
                        language: if next() % 2 == 0 {
                            "rust".into()
                        } else {
                            "python".into()
                        },
                        text: format!("fn main() {{ println!(\"{}\"); }}", topic),
                    },
                );
            }
            4 => {
                let _ = append_child(
                    &mut doc,
                    &root,
                    BlockKind::Table {
                        rows: vec![
                            vec!["列A".into(), "列B".into()],
                            vec![topic.into(), format!("值{}", next() % 100)],
                        ],
                    },
                );
            }
            5 => {
                let _ = append_child(
                    &mut doc,
                    &root,
                    BlockKind::Image {
                        src: format!("assets/pic-{}.png", next() % 10),
                        alt: format!("{topic} 图示"),
                    },
                );
            }
            6 => {
                let _ = append_child(
                    &mut doc,
                    &root,
                    BlockKind::Quote {
                        text: format!("关于 {topic} 的一句话引用"),
                    },
                );
            }
            7 => {
                let _ = append_child(
                    &mut doc,
                    &root,
                    BlockKind::HtmlEmbed {
                        html: "<p><strong>嵌入</strong>片段</p>".to_string(),
                    },
                );
            }
            8 => {
                let _ = append_child(
                    &mut doc,
                    &root,
                    BlockKind::Canvas {
                        data: CanvasBlockData {
                            rects: vec![CanvasRect {
                                id: format!("r{}", next() % 100),
                                x: (next() % 100) as f64,
                                y: (next() % 100) as f64,
                                w: 50.0,
                                h: 30.0,
                                layer: "default".into(),
                            }],
                            notes: vec![CanvasNote {
                                id: format!("n{}", next() % 100),
                                x: 10.0,
                                y: 10.0,
                                text: format!("便签：{topic}"),
                            }],
                            layers: vec!["default".into()],
                        },
                    },
                );
            }
            _ => {
                let _ = append_child(
                    &mut doc,
                    &root,
                    BlockKind::AiGenerated {
                        model: "test-model".into(),
                        prompt: format!("总结 {topic}"),
                        text: format!("AI 生成的 {topic} 摘要"),
                    },
                );
            }
        }
    }
    doc
}

// ----------------------------------------------------------------------------
// 单元测试（基础契约）
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_tree_operations_basic() {
        let mut doc = new_doc("d1", "标题");
        let root = doc.root.clone();
        let a = add_block(
            &mut doc,
            &root,
            BlockKind::Paragraph { text: "A".into() },
            BTreeMap::new(),
        )
        .unwrap();
        let b = add_block(
            &mut doc,
            &root,
            BlockKind::Paragraph { text: "B".into() },
            BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            doc.blocks.get(&root).unwrap().children,
            vec![a.clone(), b.clone()]
        );

        // 嵌套
        let list = add_block(
            &mut doc,
            &root,
            BlockKind::List { ordered: false },
            BTreeMap::new(),
        )
        .unwrap();
        let item = add_block(
            &mut doc,
            &list,
            BlockKind::ListItem { text: "x".into() },
            BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(doc.blocks.get(&list).unwrap().children, vec![item.clone()]);

        // 移动：把 B 移到 list 下
        move_block(&mut doc, &b, &list, None).unwrap();
        assert_eq!(
            doc.blocks.get(&list).unwrap().children,
            vec![item.clone(), b.clone()]
        );
        assert!(!doc.blocks.get(&root).unwrap().children.contains(&b));

        // 环检测
        assert!(move_block(&mut doc, &list, &b, None).is_err());
        assert!(move_block(&mut doc, &root, &b, None).is_err());

        // 删除子树
        let removed = remove_block(&mut doc, &list).unwrap();
        assert!(removed.contains(&list) && removed.contains(&item) && removed.contains(&b));
        assert!(!doc.blocks.contains_key(&list));
    }

    #[test]
    fn sanitize_html_strips_scripts_and_events() {
        let dirty = r#"<p onclick="evil()">hi <script>alert(1)</script></p><a href="javascript:alert(2)">x</a><img src="data:image/png;base64,xx"><img src="https://ok.example/a.png" alt="ok"><b style="color:red">b</b>"#;
        let clean = sanitize_html(dirty);
        assert!(!clean.contains("<script"), "脚本必须剥离");
        assert!(!clean.contains("onclick"), "事件属性必须剥离");
        assert!(!clean.contains("javascript:"), "危险协议必须剥离");
        assert!(!clean.contains("data:"), "data: URL 必须剥离");
        assert!(!clean.contains("style="), "style 属性剥离");
        assert!(clean.contains("<p>"), "白名单标签保留");
        assert!(clean.contains("https://ok.example/a.png"), "安全 URL 保留");
        assert!(clean.contains("<img src=\"https://ok.example/a.png\" alt=\"ok\">"));
    }

    #[test]
    fn md_roundtrip_common_elements() {
        let md = "# 标题\n\n一段正文。\n\n- 条目一\n- 条目二\n\n1. 有序一\n2. 有序二\n\n```rust\nfn main() {}\n```\n\n> 引用语\n\n| 列A | 列B |\n| --- | --- |\n| 1 | 2 |\n\n![图](assets/pic.png)\n\n<p><strong>嵌入</strong></p>\n";
        let doc = md_to_doc("d1", "t", md);
        let out = doc_to_md(&doc);
        // 再解析一次，结构与首次一致（两遍往返不动点）
        let doc2 = md_to_doc("d2", "t", &out);
        let kinds1: Vec<(String, String)> = walk(&doc, &doc.root)
            .iter()
            .map(|b| (format!("{:?}", b.kind), block_text(&doc, b)))
            .collect();
        let kinds2: Vec<(String, String)> = walk(&doc2, &doc2.root)
            .iter()
            .map(|b| (format!("{:?}", b.kind), block_text(&doc2, b)))
            .collect();
        assert_eq!(kinds1, kinds2, "MD 往返应为不动点");
    }
}
