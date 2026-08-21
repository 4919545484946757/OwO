# STATUS-agent4.md — Agent 4 状态（R7：可靠性与可观测性 Wave 2）

> 只写本文件。任务来源：主控 R7 四 Agent 分工指令（Agent 4）。
> 当前阶段：**开发完成，scoped 门禁全绿**。

## 完成清单

### 子任务 1：event_stream.rs 扩展（指标钩子）
- `MetricsSample`（快照 + 增量字段，`to_json()` JSON 桥接）+ `MetricsObserver` 注册机制：
  `set_metrics_observer` / `reset_metrics_observer_for_test`。
- 内部调用钩子：连接打开（subscribe）、关闭（close）、发布（publish，含队列深度采样）、
  丢弃（mergeable/critical）、慢消费者 lagged（lagged_total 计数）均发出快照样本。
- **锁安全修复**：指标样本统一在 queue/subscribers 锁释放后发出（原实现会在持锁时
  进入 `sample()` 造成非重入 Mutex 死锁）。
- `EventStreamStats` 扩展：`connections_opened_total`、`lagged_total`。
- 未修改 sse.rs / lib.rs。

### 子任务 2：observability_api.rs 扩展（消费钩子 + /metrics/slo）
- `ingest_metrics_sample(&Value)`：解析 event_stream 样本并更新运行时注册表（与 event_stream
  类型解耦）——`/metrics/runtime` 现可反映真实运行期值（e2e 测试证明非零）。
- `RuntimeMetrics` 增加 `sse_lagged`；`record_event_lagged(n)` 独立计数。
- 新端点 `GET /metrics/slo`：`SloReportProbe` 回调注册（`register_slo_report_probe(Arc::new(...))`，
  未注册返回空报告不 panic）；路由已并入 `observability_api::router`。

### 子任务 3：slo.rs（新建）
- 按综合文档 §6.5 定义 5 项 SLO：ipc(<5ms)、tool_schedule(<10ms)、panel_wake(<150ms)、
  http_success(≥99.9%)、audit_zero_loss(100%)。
- `check_slo`/`error_budget`/`report`；错误预算违规判定与 record 达标判定一致
  （延迟越界也算 bad，非仅 ok=false）；全局注册表 Mutex<Option> 可重复 reset；
  `SloState` 内部 Arc 共享（get 克隆写同一份数据）。

### 子任务 4：scripts/soak.ps1（新建）
- 短模式（默认 10 分钟；`-Seconds N` 冒烟）/ `-Long`（60 分钟）跑
  "感知→定位→执行→验证→学习"六请求循环；每轮采集目标进程 RSS/句柄，
  卡死断言；首轮 baseline.json + 终局/中断 finally summary.json + rounds.csv。
- 严格前缀白名单 `%TEMP%\owo-soak-*`；退出码 0/1；已加 UTF-8 BOM（PS5.1 兼容）。
- 冒烟实测：15 秒短跑可启动、可自然结束、summary 落盘。

### 子任务 5：observability.panel.js（SLO 区块）
- SSE 慢消费者断开计数 + "SLO 基线（Wave 2）"表格（目标/p95/成功率/样本/违规预算/达标状态）。

## 门禁实测（scoped 全绿）

| 门禁 | 命令 | 结果 |
|---|---|---|
| 事件流测试 | `cargo test -p owo-agent-server --test event_stream_tests` | ✅ 20/20（12 R6 + 8 R7） |
| 可观测性测试 | `cargo test -p owo-agent-server --test observability_tests` | ✅ 22/22（13 R6 + 9 R7，含端到端桥接） |
| SLO 测试 | `cargo test -p owo-agent-server --test slo_tests` | ✅ 14/14（全部新增） |
| node | `node --check desktop/web/panels/observability.panel.js` | ✅ 0 错误 |
| fmt | 我的 6 个 .rs 文件 `rustfmt --check` | ✅ 干净 |
| clippy | `cargo clippy -p owo-agent-server --all-targets -- -D warnings` | ✅ 我的文件 0 错误（剩余 2 个在 Agent 1 的 route_contract_tests.rs，见 DEPENDENCIES） |
| soak.ps1 语法 | PowerShell Parser::ParseFile | ✅ 0 错误 |
| soak 冒烟 | `-Seconds 15` 短跑 | ✅ 可启动/可结束/落盘 |
| 编码 | 10 个文件校验 | ✅ UTF-8（.rs/.js/.md 无 BOM；soak.ps1 带 BOM）；无 U+FFFD、中文完好 |

新增测试 **31 项**（8+9+14，≥24 达标）。

## 退出标准对照

- ✅ 新增用例 ≥24 且全绿（31 项）；
- ✅ 模拟流量后 `/metrics/runtime` 的 SSE 连接/事件/队列深度为真实非零值
  （`e2e_event_stream_feeds_runtime_metrics`：真实 hub → observer → ingest → GET 断言）；
- ✅ `/metrics/slo` 契约测试通过，错误预算可计算（`slo_endpoint_error_budget_computable`）；
- ✅ soak 短模式可启动、可中断（finally 落盘）、无越界写（前缀白名单），RSS/句柄基线落盘；
- ✅ 未修改任何冻结文件（lib.rs/route_contract_tests.rs/sse.rs/gate.ps1 均未触碰）；
- ✅ 新增文件 UTF-8 无 BOM（soak.ps1 除外，PS5.1 兼容需要，与 gate.ps1 先例一致）。

## 需主控接线的点

1. **lib.rs 接线（两行）**：`pub mod slo;` + 注册探针与指标桥接（示例见 DEPENDENCIES）。
2. **route_contract_tests**：`GET /metrics/slo`（GET 白名单）；`/metrics/runtime` 补 `sse.lagged_total`。
3. **OpenAPI 登记**：`/metrics/slo`；`/metrics/runtime` 响应补字段。

## 遗留风险

- route_contract_tests.rs 的 clippy 2 错误（await_holding_lock / manual_range_contains）与
  fmt diff 在 Agent 1 文件中，需其收尾（本 Agent 已按约定不修改）。
- SLO 数据面经探针注册接入——主控未接线时 `/metrics/slo` 返回空报告（不 panic）。
- soak 无 server 时 HTTP 全失败计入（脚本设计前提：先起 server）。
