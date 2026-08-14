# STATUS-A1.md — Agent A1 状态（M4a 云端执行正式化）

> 我只写本文件。任务来源：主控 2026-08-14 M4 分工（Agent A1）。
> 白名单：`core/src/cloud_exec.rs`、`core/tests/cloud_exec_tests.rs`、`crates/owo-agent-cli/src/main.rs`、`scripts/cloud-*.py|ps1`（新建）。

## 认领

- 2026-08-14：M4a 云端执行 v0.1→v0.2 正式化。

## 里程碑清单（全部完成）

### P1-1 传输后端抽象
- `CloudTransport` trait（submit/status/fetch_result/cancel + kind()）：`MockRemoteTransport`（不联网替身，本地隔离目录模拟远端，提交即执行完）+ `HttpTransport`（reqwest，协议契约写入模块头注释，供主控接 server 参考：POST /cloud/tasks、GET /cloud/tasks/{id}、GET /cloud/tasks/{id}/result、POST /cloud/tasks/{id}/cancel；https 明确拒绝）。
- 凭据：`cloud_token_from_env()` 只读 `OWO_CLOUD_TOKEN`/`OWO_CLOUD_API_KEY`，仅进 Authorization 请求头；所有结构体/持久化 JSON 不含凭据（契约测试断言）。

### P1-2 任务队列与状态机
- `TaskState`：Queued→Running→Succeeded/Failed/Canceled；`TaskRecord`（唯一持久化结构）。
- `CloudTaskQueue`：submit（校验+入队+持久化）、run_next（取首个 Queued，经传输执行）、retry（手工重试）、cancel（远端 cancel+置 Canceled）、recover（重启恢复，Running 重置为 Queued 可重跑）。
- 持久化：`<dir>/<task_id>.json`（serde pretty）；task_id 按现有最大编号递增避免冲突。
- 重试：`retry_count` 累加 + `backoff_delay(base, n) = base*2^n` 封顶 60s（纯函数）；未超 `max_retries` 回 Queued，超限 Failed。

### P1-3 进度事件流
- `CloudProgress` 枚举（Snapshotting/Submitting/Submitted/Executing/Fetching/Retrying/Succeeded/Failed/Canceled）；`ProgressSink` trait + `NullSink` + `CollectingSink`（测试）+ `tokio::mpsc::UnboundedSender` 适配（内存 channel；SSE 接入留给主控）。

### P1-4 完整闭环
- 端到端测试：MockRemote 提交→执行改文件→diff 回传→本地 apply→revert 往返恢复原状；任务间隔离（两个任务同路径文件互不覆盖）；审计覆盖 submit/run/apply/revert/cancel/retry 全动作。

### P1-5 CLI
- `owo-agent cloud`：submit（--command 可重复/--run 立即执行/--timeout）、list、status、diff、apply、revert；--transport mock|http（--url 必填）；队列目录 --dir（默认 %TEMP%）。--help 正常；真实网络失败输出含 URL 与建议的清晰报错。

### P1-6 安全
- `validate_commands`：危险模式黑名单（rm -rf /、format c:、del /s、shutdown 等）恒拒绝 + 可选命令前缀白名单；队列提交层同校验。
- 超时熔断：远端轮询预算 = timeout×2，超时即失败；凭据永不落盘（测试断言持久化 JSON 与审计无 token/Authorization/secret）。

## 实测命令与输出

- `cargo test -p owo-agent-core --test cloud_exec_tests` → **16 passed**（v0.1 7 项 + v0.2 新增 9 项）
- `cargo fmt -p owo-agent-core -p owo-agent-cli -- --check` → 干净
- `cargo clippy -p owo-agent-core --all-targets -- -D warnings` → 0 警告
- `cargo clippy -p owo-agent-cli --all-targets -- -D warnings` → 0 警告
- `cargo check -p owo-agent-core` / `-p owo-agent-cli` → 通过（A4 computer_use.rs 中途震荡后已稳定，最终全绿）
- CLI 端到端（mock）：submit --run → cloud-0001 Succeeded diff=2；list 显示；diff 列出 Modified a.txt/Added b.txt；apply 后 a.txt=new；revert 后 a.txt=old；原工作区 apply 前未改动
- CLI HTTP 真实失败：`HTTP POST http://127.0.0.1:59999/cloud/tasks 失败（请检查远端地址/网络）：error sending request...`（清晰报错 ✓）

## 新增测试清单（cloud_exec_tests.rs v0.2 区）

1. `v02_end_to_end_mock_remote_apply_revert` — 全链路闭环 + 进度事件序列 + 审计
2. `v02_credentials_never_persist` — 凭据不落盘（持久化 JSON/审计无 token）
3. `v02_queue_recover_after_restart` — 队列重启恢复 + Queued 续跑
4. `v02_running_record_recovered_as_queued` — Running→Queued 重置
5. `v02_retry_backoff_and_exhaustion` — 退避纯函数 + 超限 Failed + 未超限回 Queued
6. `v02_command_allowlist_and_dangerous_rejected` — 危险/越权命令拒绝
7. `v02_cancel_and_isolation` — 取消 + 任务隔离
8. `v02_http_transport_unreachable_clear_error` — 真实网络失败清晰报错 + https 拒绝
9. `v02_http_transport_contract_against_inline_server` — 极简 HTTP 远端全契约 + Authorization 头透传

## 给主控的接入说明（HTTP 面）

- 协议契约见 `cloud_exec.rs` 模块头注释（4 个端点，JSON）。
- server 接入时：`CloudTaskQueue::new(dir, Box::new(HttpTransport::new(url)?))` + `/cloud/*` 路由；SSE 进度可用 `ProgressSink` for `mpsc::UnboundedSender<CloudProgress>` 适配。
- 队列目录建议放 `data_root/cloud/queue`。

## 遗留问题 / 依赖

- 无新增依赖（reqwest 已在 core；CLI 用现有 clap）。DEPENDENCIES.md 无需请求。
- P2（断线重连、diff 多文件合并展示、成本计量）未做，留待下轮。
