# STATUS-T1.md — Agent T1 状态（M4c 多格式笔记 v1）

> 我只写本文件。任务来源：主控 2026-08-14 第三轮分工（T1）。
> 白名单：`core/src/notes.rs`（新）、`core/tests/notes_tests.rs`（新）。
> 禁止：lib.rs/Cargo.toml/server/CLI/desktop（lib.rs 的 `pub mod notes;` 属主控计划内动作，T1 本地已先行添加用于验证，见下）。

## 认领

- 2026-08-14：从零落地技术文档 §5.6 多格式文档模型 v1——文档=块树，MD/HTML/画布是同一模型的渲染器。

## P1 里程碑完成情况（全部完成）

| # | 里程碑 | 实现 | 测试 |
|---|---|---|---|
| 1 | 块树模型 | `NoteDoc{id,title,root,blocks,updated_at}` + `Block{id,kind,attrs,children}`；BlockKind 覆盖 段落/标题/列表/代码/表格/图片/文件/引用/HTML嵌入/画布/AI生成；纯函数操作 add/insert/remove(子树)/move(环检测)/append + walk 遍历 | 5 项 |
| 2 | 持久化 | `save_doc`（doc.json 原子写 + assets/ 目录）/ `load_doc`；serde 往返 + 磁盘重开 | 4 项 |
| 3 | 导入导出 | `md_to_doc`/`doc_to_md`（段落/标题/列表/代码/表格/图片/引用/HTML 嵌入）；`sanitize_html`（标签白名单 + 禁标签整体剥离 + 事件属性/危险 URL/style 剥离）；MD 往返不动点 + 50 组程序化样例 | 8 项 |
| 4 | 画布块 | `CanvasBlockData{rects,notes,layers}` serde/磁盘往返 | 1 项 |
| 5 | 全文索引 | `NoteIndex` trait：`InMemoryNoteIndex`（分词）+ `FtsNoteIndex`（SQLite FTS5 trigram tokenizer，中文子串检索；<3 字符查询 LIKE 回退）；`NoteIndexer`/`search_notes` | 4 项 |
| 6 | 零丢失验收 | 10 次"改→存→读"哈希稳定 + MD↔块往返结构一致 | 2 项 |
| 7 | 100 份混合样例 | `generate_mixed_doc(seed)` 确定性生成（11 类块）；100 份磁盘+MD 往返 | 2 项（含 50 组样例） |

## 实测结果（2026-08-14 21:3x）

- `cargo test -p owo-agent-core --test notes_tests` → **27/27 通过**（含 lib 内 3 个单测为 notes_tests 独立 24 项？——实际 notes_tests.rs 27 个测试函数全绿）
- `rustfmt` 单文件格式修复完成（仅动自己的两个文件；他人文件格式差异未触碰）
- clippy：notes.rs 相关 lint 已清零（`cargo clippy -p owo-agent-core --all-targets` 当前被 T2 的 plugin.rs 依赖阻塞，见下）
- ⚠️ **当前阻塞**：T2 的 plugin.rs 引用了 `sha2`/`ed25519_dalek`（新依赖，已由 T2 在 DEPENDENCIES.md 留言请求主控添加），主控尚未合并 → core crate 整体编译失败 → T1 门禁复跑（fmt/clippy/全量测试）被阻塞。T1 主体代码已完成且验证通过（27/27）。

## 协作说明

- lib.rs：本地已加 `pub mod notes;`（主控计划内单行，与主控收尾合并无冲突）；T4 也请求了 `pub mod goal;`/`pub mod plan;`（DEPENDENCIES.md 有留言）。
- 无新增依赖请求（rusqlite 已在 core；trigram tokenizer 由 bundled SQLite 提供）。

## 公开 API 清单（给主控：lib.rs 需导出）

```rust
pub mod notes;  // 已包含全部；可选 pub use notes::{...}

pub use notes::{
    // 块树
    Block, BlockId, BlockKind, NoteDoc,
    // 操作
    add_block, append_child, block_text, doc_title, get_block, insert_child, move_block,
    new_doc, remove_block, walk,
    // 持久化
    load_doc, save_doc,
    // 导入导出
    doc_to_md, md_to_doc, sanitize_html,
    // 画布
    CanvasBlockData, CanvasNote, CanvasRect,
    // 索引
    FtsNoteIndex, InMemoryNoteIndex, NoteIndex, NoteIndexer, SearchHit, search_notes,
    // 样例生成器（测试辅助）
    generate_mixed_doc,
};
```

## 数据模型说明

- `NoteDoc{ id, title, root: BlockId, blocks: BTreeMap<BlockId, Block>, updated_at }`；唯一根"root"；children 有序。
- `BlockKind` 结构化字段（表格 rows / 代码 language+text / 图片 src+alt / 文件 path+mime / 画布 data / AI model+prompt+text）；通用 `attrs: BTreeMap<String, Value>` 兜底扩展。
- 画布：`CanvasBlockData{ rects: [{id,x,y,w,h,layer}], notes: [{id,x,y,text}], layers: [String] }`（数据往返保证，渲染留前端）。
- 索引：`NoteIndex` trait（index_doc 全量重建幂等 + search）；FTS5 trigram 对中文子串友好；<3 字符查询 LIKE 回退。
- MD 渲染边界：Canvas/AiGenerated 块不进 Markdown（v1 契约：MD 为"可表达元素"渲染器，这两类数据保真由 doc.json 承担）。

## 遗留问题

- P2 未做：embedding/RAG 检索钩子（`NoteIndex` trait 已预留扩展点）、Yjs CRDT 骨架、批量 HTML 渲染。
- MD 内联样式（**bold** 等）v1 不解析，原样保留为文本。
- 待主控处理 T2 依赖后复跑全量门禁确认无回归。
