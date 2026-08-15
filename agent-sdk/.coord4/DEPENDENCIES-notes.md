# DEPENDENCIES-notes.md — Agent A（Lane A）依赖/共享文件请求

> 需要碰共享文件或新增依赖时留言 @主控，停手等待。

## 依赖请求

- 无新增 crate 依赖（axum/serde/serde_json/tokio/uuid/chrono 均已在 server；tempfile/tower 已在 dev-deps）。

## 对主控/其他 lane 的依赖

1. **lib.rs 接线**：`mod notes_api;` + `build_router` 合并 `notes_api::router(state)`（唯一串行点）。
2. **openapi_spec / route_contract_tests.rs / index.html / app.js**：登记与挂载（详见 STATUS-notes.md"需主控接线的点"）。
3. 与其他 lane 无文件交集；`notes_api.rs` 独立编译，不依赖 sse.rs/goal_api.rs 等。

## 测试隔离说明

- 测试仅用 tempfile 临时目录（data_root=tempdir），不触碰真实 data_root/workspace。
- 模块级 `STORES` 单例按 data_root 键控，测试间互不污染；Windows 下删除前已先释放 FTS SQLite 句柄（否则 remove_dir_all 失败）。
