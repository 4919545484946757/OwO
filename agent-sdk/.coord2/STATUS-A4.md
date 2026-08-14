# STATUS-A4.md — Agent A4 状态（M4d computer-use 审批版闭环）

> 我只写本文件。任务来源：主控 2026-08-14 M4 四线分工指令 + 遗留问题清理轮。

## 认领

- 时间：2026-08-14
- 任务：computer_use.rs + computer_task.rs 推进为文档 §7.3/§9 M4d 审批版闭环。先 P1（已完成）；后续按主控指令解决全部遗留问题（HTTP 接线、真实桌面 surface、https、P2 cloud_exec）。
- 白名单：core/src/computer_use.rs、core/src/computer_task.rs、core/tests/computer_use_tests.rs、scripts/computer-use-e2e.py；遗留清理轮经主控授权追加：server/lib.rs、route_contract_tests.rs、core/src/cloud_exec.rs、core/tests/cloud_exec_tests.rs。
- 禁止项遵守：未改 Cargo.toml/lock、core/src/lib.rs、cli/main.rs、他人文件；未 commit。

## P1 里程碑（上轮已完成，摘要）

1. 动作门禁 `task_gate_check` + 5 个 `desktop_*_gated` 入口；2. 敏感熔断 `scan_ui_sensitive`→Fused→resume；3. 感知闭环 `run_approved_task(_on)` + TaskSurface；4. 契约测试 11 项；5. 超时+动作预算（registry 侧）。

## 遗留问题清理轮（本轮）

| 遗留项 | 处理 | 证据 |
|---|---|---|
| HTTP 接线（run 端点 + gated handler + /cloud/*） | ✅ server/lib.rs 追加：POST /computer-use/task/{id}/run、desktop_* 可选 task_id+sensitive 门禁、/cloud/tasks 四端点（A1 协议契约）；openapi_spec +5 路径（111 总）；route_contract 白名单 +3 资源型路径；快照重抓 | 运行时实测：/cloud/tasks 提交→Succeeded（diff 2、duration_ms 69）；run 端点未批准 403、批准后 4 步闭环 Completed；server 测试 3/3 |
| 真实桌面 surface | ✅ `RealTaskSurface`（ocr_screen/executor），run 端点无 OWO_SIM_QQ_URL 时自动走真实面 | 编译 + 契约测试通过 |
| https 传输 | ✅ HttpTransport 接受 https（reqwest default-tls），scheme 校验 {http,https} | `p02_https_transport_accepted_and_scheme_validated` |
| P2 断线重连 | ✅ 轮询/拉取瞬时错误退避重试（POLL_RETRY_MAX=4）+ Retrying 事件 | `p02_poll_reconnect_retries_transient_errors` |
| P2 多文件合并 diff | ✅ `describe_diff` 摘要 + `validate_batch` zip-slip 防护（apply_to/revert_from 前置校验） | `p02_diff_describe_and_path_validation`、`p02_cloud_result_batch_apply_revert_with_escape_guard` |
| P2 成本/时长计量 | ✅ TaskRecord.duration_ms + `queue.usage()`（duration/diff_count/retry_count） | `p02_usage_metrics_duration_and_diff_count` |
| A1 fmt/clippy | ✅ 已由 A1 修复（全量门禁 0 警告） | clippy --workspace --all-targets 0 警告 |
| server max_actions 同步 | ✅ 预算表在 registry 侧（上轮已解决），本轮核实无回潮 | computer_task.rs 无 max_actions 字段；server 编译通过 |
| ProgressSink Sync | ✅ 补 `Send + Sync`（&dyn ProgressSink 跨线程） | 全量编译通过 |

## 门禁实测（2026-08-14 收尾）

| 门禁 | 命令 | 结果 |
|---|---|---|
| workspace 全量 | `cargo test --workspace` | ✅ 323 项全绿（21 suites，0 failed） |
| A4 契约 | `cargo test -p owo-agent-core --test computer_use_tests` | ✅ 11/11 |
| cloud P2 | `cargo test -p owo-agent-core --test cloud_exec_tests` | ✅ 21/21（16 + 5 P2） |
| server | `cargo test -p owo-agent-server` | ✅ 3/3（route_contract 覆盖新路径） |
| fmt | `cargo fmt --all -- --check` | ✅ 干净 |
| clippy | `cargo clippy --workspace --all-targets -- -D warnings` | ✅ 0 警告 |
| TS SDK | `npm run generate:local` + `typecheck` | ✅ schema.d.ts 已随 111 路径快照重生成，0 错误 |
| 运行时矩阵 | curl（模拟面 + mock 传输） | ✅ /cloud/tasks 全链、run 端点 403/200、cancel/result/status 语义正确 |

## 产出文件（本轮新增/修改）

- `server/lib.rs`：computer_task_run、gate_desktop_action、cloud_queue/cloud_task_*、desktop 请求体 task_id+sensitive、openapi_spec +5 路径、AppState.cloud_queue。
- `core/src/computer_use.rs`：`sim_base_url_configured()`、`RealTaskSurface`、`TaskGoal` 加 Deserialize、`TaskSurface: Send`。
- `core/src/cloud_exec.rs`：https、断线重连、describe_diff、validate_batch、UsageMetrics、TaskRecord.duration_ms、queue.usage()、run_next pub、ProgressSink Sync。
- `core/src/audit.rs`：AuditLog 加 Clone（run 端点 scratch 审计合并）。
- `core/tests/cloud_exec_tests.rs`：+5 P2 测试、https 测试语义更新。
- `server/tests/route_contract_tests.rs`：+3 资源型白名单。
- `clients/ts/openapi.json`：重抓（111 路径）；`clients/ts/src/schema.d.ts`：重新生成。
- `.coord2/STATUS-A4.md`、`.coord2/DEPENDENCIES.md` 更新。

## 遗留问题（本轮后剩余）

1. SSE 进度推送（ProgressSink → mpsc → /cloud/tasks/{id}/events）未接（A1 原始约定归主控；HTTP 面已可用轮询）。
2. desktop_shortcut 未纳入门禁动作集（不在 P1 清单内；如需可后续加 desktop_shortcut_gated）。
3. `computer_task_run` 为同步等待闭环完成；超长任务可后续改后台执行 + 状态轮询（TaskReport 已含 state）。
4. 真实桌面闭环需人工在交互会话启动服务（沙箱无输入桌面，环境限制，非代码缺陷）。
