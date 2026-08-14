# DEPENDENCIES.md — 依赖/共享文件请求留言区（主控维护）

> 需要新增依赖、请求改共享文件（server/lib.rs、Cargo.toml/Cargo.lock、owo-agent-cli/Cargo.toml、core/lib.rs）时在此留言 @主控。

## 留言

- 2026-08-14 A4：本轮 computer-use 审批版无需新增依赖（复用 tokio/reqwest/async-trait/chrono，均为既有依赖）。
- 2026-08-14 A4：`ComputerTask` 结构体未新增字段（动作预算落在 ComputerTaskRegistry 侧），server/lib.rs 现有 `ComputerTask { ... }` 构造点不受影响。
- 2026-08-14 A4（遗留清理轮）：
  1. **HTTP 接线已完成**（主控授权 A4 代做，全部在 server/lib.rs 内追加，无既有路由改动）：
     - `POST /computer-use/task/{id}/run`：闭环执行（模拟面 OWO_SIM_QQ_URL → SimTaskSurface，否则 RealTaskSurface）；未批准/门禁失败 → 403；审计经 scratch 合并（std MutexGuard 不可跨 await）。
     - `desktop_click/type/key/scroll/launch` 请求体新增可选 `task_id` + `sensitive{name,role,ocr_text}`：携带 task_id 时先过 `task_gate_check`（拒绝 403 + 审计），不带则保持旧行为（兼容既有调用方）。
     - `/cloud/*` 四端点（POST /cloud/tasks、GET /cloud/tasks/{id}、GET /cloud/tasks/{id}/result、POST /cloud/tasks/{id}/cancel），按 A1 协议契约接线：懒初始化 `CloudTaskQueue`（队列目录 data_root/cloud/queue；传输 = OWO_CLOUD_BASE_URL → HttpTransport，缺省 MockRemoteTransport 本地模拟）。已补 openapi_spec 5 路径 + route_contract 白名单 + 快照重抓（111 路径）。
  2. **真实桌面 surface**：`RealTaskSurface`（OCR 走本地引擎、动作走 executor）已实现；`/computer-use/task/{id}/run` 在无 OWO_SIM_QQ_URL 时自动走真实面。
  3. **https**：HttpTransport 不再拒绝 https（reqwest 内置 default-tls），仅校验 scheme ∈ {http,https}；`v02_http_transport_unreachable_clear_error` 已按新语义更新。
  4. **P2 cloud_exec**：断线重连（轮询/拉取瞬时错误退避重试 ≤4 次 + Retrying 事件）、多文件合并（`describe_diff` 摘要 + `validate_batch` zip-slip 防护，apply_to/revert_from 前置校验）、成本/时长计量（`TaskRecord.duration_ms` + `queue.usage()`）；新增 5 项契约测试（cloud_exec_tests 21/21）。
  5. **A1 fmt/clippy**：cloud_exec.rs 与 cloud_exec_tests.rs 的 fmt/clippy 差异已由 A1 自行修复（本轮全量门禁 0 警告）。
  6. `ProgressSink` trait 增加 `Sync` 边界（`&dyn ProgressSink` 跨线程必需）。
- 2026-08-14 A4 → @A1：本轮对 cloud_exec.rs/cloud_exec_tests.rs 做了 P2 扩展与 https 语义调整（见上），你如继续迭代请注意：`TaskRecord` 新增 `duration_ms`（serde default 兼容旧 JSON）、`run_next` 已改 pub、`ProgressSink: Send + Sync`。
