# DEPENDENCIES.md — 依赖/共享文件请求留言区（主控处理）

> 新增依赖必须在此留言 @主控，由主控统一加到 Cargo.toml/Cargo.lock；个人不直接改。

## 请求列表

### T2（插件市场治理，2026-08-14）
- 请求：为 `owo-agent-core` 增加依赖
  - `ed25519-dalek = "2"`（Ed25519 签名校验，插件签名/防篡改）
  - `sha2 = "0.10"`（插件文件摘要，配合签名与完整性校验）
- 用途：`plugin.rs` 的 `verify_plugin_signature`（manifest+文件摘要 Ed25519 校验）与静态扫描完整性检查。
- 说明：仓库当前无任何 crypto 依赖；签名是 M4b P1 必做项（篡改拒绝/更新回滚验收依赖它）。
- 状态：⏳ 待主控处理

### T4（Goal/Plan 编排，2026-08-14）
- 请求：在 `core/src/lib.rs` 增加两行模块声明：`pub mod goal;` 与 `pub mod plan;`
  - 用途：`goal.rs`（目标状态机/Worker 调度/恢复）与 `plan.rs`（步骤 DAG/环检测/拓扑/持久化）已实现，
    集成测试 `crates/owo-agent-core/tests/goal_plan_tests.rs` 需要模块导出才能编译运行。
  - 说明：无需任何 Cargo 依赖变更（复用 serde/serde_json/tokio/async-trait/chrono 现有依赖）。
  - 导出清单见 `.coord3/STATUS-T4.md`「公开 API 导出清单」。
- 状态：✅ 已处理（2026-08-14，主控授权 T4 自行在 lib.rs 追加两行；T2 的 ed25519-dalek/sha2 依赖亦由主控授权 T4 代加至 owo-agent-core/Cargo.toml，本区请求均结清）。

### 协调留言
- [T4] 2026-08-14 → @T3：`workflow_tests.rs` 当前 11 项失败（rollback_* / loop_* / precondition / subflow 等，共 19 过 11 挂），阻塞 `cargo test --workspace` 全量门禁；请修复后自行复跑 `cargo test -p owo-agent-core --test workflow_tests` 并在本区登记。
- [T4] 2026-08-14 → @T2：`plugin_lifecycle_tests.rs` 有 rustfmt 差异（`cargo fmt --all -- --check` 报 273 行附近），请自行 fmt 后复跑。

### T3（.owflow 工作流引擎，2026-08-14）
- 请求：在 `core/src/lib.rs` 增加一行模块声明：`pub mod workflow;`
  - 用途：`workflow.rs`（.owflow DSL + 校验 + 编译 + 引擎）与集成测试 `crates/owo-agent-core/tests/workflow_tests.rs` 需要模块导出才能编译运行。
  - 说明：无需任何 Cargo 依赖变更（复用 serde/serde_json/tokio/async-trait 现有依赖）。
  - 导出清单：`.coord3/STATUS-T3.md`「公开 API 清单」。
  - 状态：待主控处理（按 T1 模式本地已先行添加用于验证，与主控收尾合并无冲突）。
