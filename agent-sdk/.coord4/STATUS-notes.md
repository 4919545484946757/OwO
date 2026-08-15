# STATUS-notes.md — Agent A（Lane A：笔记 HTTP API + 桌面笔记面板）

> 我只写本文件。第四轮"核心模块 HTTP/UI 集成轮"。

## 交付文件（仅新建，未改任何既有文件）

- `crates/owo-agent-server/src/notes_api.rs`（新）
- `crates/owo-agent-server/tests/notes_api_tests.rs`（新）
- `desktop/web/panels/notes.panel.js`（新）
- 本文件 + `DEPENDENCIES-notes.md`

## 完成清单

### 路由（前缀 /notes，全部在 `notes_api::router` 内注册，独立编译、无 crate::/super::）

- `GET /notes` 列表（index.json 清单，损坏时扫描重建）
- `POST /notes {title, markdown?}` → 201（markdown 走 `md_to_doc`）
- `GET /notes/{id}` 整文档块树
- `PUT /notes/{id} {title?, blocks?}` 整文档替换（校验：root 存在 / children 引用存在 / 无孤儿块）
- `DELETE /notes/{id}`
- `POST /notes/{id}/blocks {parent?, after?, kind, text?, data?}` 添加块（11 种 kind 解析：paragraph/heading/list/list_item/code/table/image/file/quote/html/canvas/ai；html 入库前 sanitize）
- `PATCH /notes/{id}/blocks/{block_id} {text?, data?}`（按现有 kind 更新字段）
- `DELETE /notes/{id}/blocks/{block_id}` 返回被删子树 id 列表
- `POST /notes/{id}/blocks/move {block_id, parent?, after?}`（环检测复用 core move_block）
- `POST /notes/import {title, markdown}`
- `GET /notes/{id}/export/{format}`（md=doc_to_md；html=块树渲染+转义+sanitize_html 兜底）
- `GET /notes/search?q=`（跨文档合并检索）
- `POST /notes/{id}/reindex`

### 存储（协议约束：不给 AppState 加字段 → data_root 键控模块内单例）

- `OnceLock<Mutex<HashMap<data_root, Arc<Mutex<NoteStore>>>>>`；`<data_root>/notes/<id>/doc.json`（save_doc/load_doc）+ `index.json` 清单 + `<id>/fts.db`（每文档 FTS5 索引，写操作后重索引该文档，搜索遍历合并）
- 设计决策说明：core `FtsNoteIndex::index_doc` 语义为"单文档重建（DELETE ALL + 插入）"，多文档场景下每文档独立 fts.db 可避免相互覆盖
- 审计：写操作（create/update/delete/import/block add/move/update/delete）经 `state.agent.audit_log()` 落 AuditLog

### 面板 notes.panel.js

文档列表 + 新建（标题+markdown）+ 搜索回车 / 块树渲染 / 导出 md/html 下载 / 标题双击重命名 / 删除确认；helpers 缺省自行 fetch；样式 owo-notes- 前缀 + mount 注入 style；防御性降级（baseUrl 缺省 127.0.0.1:4098）。

## 门禁结果（实测）

| 门禁 | 结果 |
|---|---|
| `cargo test -p owo-agent-server --test notes_api_tests` | ✅ 13/13（创建/列表/读取/删除、400 校验、404、PUT 替换+孤儿拒绝、块增删移动层级、PATCH、错误路径 6 例、import MD 往返零丢块、导出 md/html、html 无 script/onerror、搜索命中/未中/缺 q 400、删除后搜索失效+reindex、审计 3 事件） |
| `cargo clippy -p owo-agent-server --all-targets -- -D warnings` | ✅ notes_api.rs / notes_api_tests.rs 0 警告（其余 lane 文件（sse.rs/goal_api.rs/plugin_market_api_tests.rs）编译错误为其他 Agent 进行中，非本 lane） |
| `cargo fmt --all -- --check` | ✅ 本 lane 两文件无 Diff（rustfmt 单文件已格式化） |
| `node --check desktop/web/panels/notes.panel.js` | ✅ 0 错误 |

## 需主控接线的点

1. `lib.rs`：`mod notes_api;` + `build_router` 合并 `notes_api::router(state)`（`.merge()` 或 route 前缀拼接）。
2. `openapi_spec` + `clients/ts/openapi.json`：登记 `/notes` 全部 14 条路径。
3. `route_contract_tests.rs`：为新 POST 路由补 sample_body；`{block_id}` 路径参数补 sample_path 占位；`GET /notes/{id}/export/{format}`、`GET /notes/{id}` 资源型 404 需补白名单（不存在笔记返回 404）。
4. `index.html` + `app.js`：引入 `panels/notes.panel.js` 并挂载 `OwoPanels.notes`。

## 风险/遗留

- 搜索为每文档 FTS 合并检索：文档数大时每次 search 打开多个 db（当前打开一次后缓存于注册表）；性能优化留待后续。
- `PUT /notes/{id}` 的 blocks 校验要求完整树（无孤儿），前端导入/合并场景需自行保证完整块表。
- 面板"编辑保存"当前实现为导出 MD→导入新文档（原文档保留），如需原地覆盖编辑可在下一轮补 `PUT` 调用。
