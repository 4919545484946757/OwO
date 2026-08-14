# STATUS-A.md — Agent A 状态（HTTP 服务面恢复 + 路由契约测试）

> 我只写本文件。任务来源：主控 2026-08-14 分工指令（Agent A 角色）。

## 认领

- 时间：2026-08-14
- 任务：恢复 `crates/owo-agent-server/src/lib.rs` 丢失的 v0.5 端点（以 `clients/ts/openapi.json` 快照为权威契约），补路由契约测试，杜绝回归。
- 白名单文件：`crates/owo-agent-server/src/lib.rs`、`crates/owo-agent-server/Cargo.toml`、`crates/owo-agent-server/tests/route_contract_tests.rs`、`clients/ts/*`。
- 确认：OWNERSHIP.md 由 D 冻结，lib.rs 归 A 独占，无并发写入（期间文件曾被反复改写至编译失败，最终由主控安定为可编译基线；A 未做破坏性重建）。

## 执行记录（时间戳）

- 2026-08-14 13:2x 认领登记；核对契约与现状。
- 现状核对结论：契约 73 路径已全部注册（多行 route 写法需用多行正则提取）；真正问题为：
  1. `openapi_spec` 漏登记 24 个实际路由（/desktop/* ×10、/vision/* ×4、/perception/elements、/perception/ocr/bytes、/perception/window、/perception/template/* ×5、/learn/status、/openapi.json）。
  2. `route_contract_tests.rs` 仅覆盖 ~13 个路径，无 openapi 覆盖断言。
  3. 历史上存在重复 route 注册（/memory/observations、/memory/clear、/memory/mine-skill 各 x2）与重复 fn 定义（备份区）——在 A 介入前已被基线版本修复，A 复核确认当前无重复（仅 /session/{id}/attachments 与 /automations 的 get+post 合并注册，合法）。
- 修复 openapi_spec：补齐 24 个路径登记（operationId/方法/参数与路由一致）。
- 重写 `tests/route_contract_tests.rs`：
  - `all_contract_endpoints_are_reachable`：以 include_str 快照（去 BOM）为权威，对每个契约路径+方法用 `build_router + tower::ServiceExt::oneshot` 请求；`/session/{id}/*` 系列先创建真实会话强断言非 404/405；资源型 404 白名单 7 项（/skills/{name}、/skills/{name}/enabled、/learn/packages/{name}、/learn/export/{name}、/traces/{index}、/mcp/remove、/automations/{id}/toggle）注明原因。
  - `openapi_json_covers_snapshot_and_registered_routes`：断言 /openapi.json 包含契约快照全部路径 + lib.rs 实际注册全部路由（include_str 提取 .route("...")）。
  - `v05_routes_are_registered_not_404_via_real_http`：真实端口 smoke（原测试升级，覆盖 18 个 GET + 7 个 POST）。
- Cargo.toml：dev-dependencies 增加 `tower 0.5 (util)` + `tempfile 3`（契约测试需要，不占运行时依赖）。

## 门禁结果（实测）

| 门禁 | 结果 |
|---|---|
| cargo fmt --all -- --check | ✅ 干净（仅 A 的文件有格式差异，已修复） |
| cargo clippy -p owo-agent-server --all-targets -- -D warnings | ✅ 0 警告 |
| cargo test -p owo-agent-server | ✅ 全绿（route_contract_tests 3/3 + 其他 3/3） |
| cargo test --workspace | ✅ 全绿（core 220 lib + 全部集成套件，无破坏） |
| 运行时 curl 矩阵 | ✅ 全部契约路径非 404（/session/{id}/context 对不存在会话返回 404 属资源型，白名单语义） |
| /openapi.json 一致性 | ✅ 服务端 spec 106 路径 = 磁盘快照 = schema.d.ts（generate:local 重新生成） |
| npm run generate:local | ✅ openapi-typescript 成功 |
| npm run typecheck | ✅ tsc --noEmit 0 错误 |
| npm run build | ✅ tsc + copy-schema 成功 |
| npm run test:unit | ✅ 3 pass / 0 fail |

## 产出

- `.coord/CONTRACT.md`：契约已发布（见该文件）。
- 修改文件：`crates/owo-agent-server/src/lib.rs`（openapi_spec +24 路径）、`crates/owo-agent-server/tests/route_contract_tests.rs`（全量契约测试）、`crates/owo-agent-server/Cargo.toml`（dev-deps）、`clients/ts/openapi.json`（重抓服务端 spec）、`clients/ts/src/schema.d.ts`（重新生成）、`agent-sdk/Cargo.lock`。

## 遗留问题

- `/traces/{index}`、`/learn/packages/{name}` 等资源型路径在无数据时返回 404，测试以白名单断言「路由存在」；建议后续给 traces 落盘/包存储增加空态 200 语义（非本次范围）。
- openapi.json 的 git 跟踪状态异常（HEAD 无该文件但 ls-files 有），提交方案需由 D 在 COMMIT-PLAN 中核实。
