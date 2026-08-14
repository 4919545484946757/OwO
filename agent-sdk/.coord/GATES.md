# GATES.md — 收尾门禁矩阵（Agent D 维护）

> 状态约定：`⏳ 待跑` / `✅ 通过` / `❌ 失败` / `⏭️ 跳过（原因）`。全量门禁由 D 于 2026-08-14 收尾统一执行；A/B/C 结果以各自 STATUS 为准转抄并注明证据。

## 收尾门禁矩阵（2026-08-14 实测）

| # | 门禁 | 命令 | 结果 | 实测输出摘要 | 证据 |
| --- | --- | --- | --- | --- | --- |
| 1 | Rust fmt | `cargo fmt --all -- --check` | ✅ 通过 | 干净，0 差异 | D 实测 |
| 2 | Rust clippy | `cargo clippy --workspace --all-targets -- -D warnings` | ✅ 通过 | 0 警告 | D 实测 |
| 3 | Rust test | `cargo test --workspace` | ✅ 通过 | 294 项全绿（core lib 220 + 集成 61 + server 6 + CLI 7） | D 实测，输出存 `$TEMP\cargo_test_final.txt` |
| 4 | A 路由契约测试 | `cargo test -p owo-agent-server`（含 route_contract） | ✅ 通过 | route_contract_tests 3/3；修复 1 处白名单缺 `/perception/template/{app_id}`（资源型 404）后全绿 | D 复跑 + A STATUS |
| 5 | node 语法 | `node --check desktop/web/app.js` | ✅ 通过 | 0 错误 | D 实测 |
| 6 | Python 语法 | `python -m py_compile scripts/sim-qq-observe-e2e.py scripts/sim-regression.py` | ✅ 通过 | 0 错误 | D 实测 |
| 7 | sim 回归 | `python scripts/sim-regression.py --base 4097 --sim 18500` | ✅ 通过 | qq-learn PASS + qq-observe PASS（2/2）；首次跑因服务未带 OWO_SIM_QQ_URL 污染失败，干净环境复跑全过 | D 实测（owo-sim-qq + owo-agent serve 4097，临时数据目录） |
| 8 | 技能门禁 | `powershell scripts/skill-gate.ps1` | ✅ 通过 | 四技能 12/12 PASS（documents/spreadsheets/pdf/browser ×3） | D 实测 |
| 9 | 契约一致性 | /openapi.json 106 路径 = 磁盘快照 = schema.d.ts；路由非 404 矩阵 | ✅ 通过 | route_contract 覆盖断言 + 真实 HTTP smoke 通过；B 33 项接口矩阵 ALL PASS | A/B STATUS + D 复跑 |
| 10 | 前端面板矩阵 | 面板接口非 404 矩阵 | ✅ 通过 | 33 项 ALL PASS（资源型 404 按契约语义通过）；friendlyError 友好错误 | B STATUS |
| 11 | 打包自检 | 解包便携包三项 | ✅ 通过 | /health 200；onnx_models_present=true；OWO_OCR_STRICT=onnx 下 POST /perception/ocr/bytes provider=onnx-v4 文本非空 | C STATUS（产物时间戳 2026-08-14） |
| 12 | TS SDK | `npm run typecheck / build / test:unit` | ✅ 通过 | typecheck 0 错误；build 成功；test:unit 3/3 | A STATUS + D 复跑 |
| 13 | 外部依赖门禁 | eval-gate（需 OPENAI_API_KEY） | ⏭️ 跳过 | 本轮无凭据环境，标注跳过不虚标；历史 20/20=100%（2026-08-13） | C.4 外部验收项保持"开放" |

## 依赖顺序与外部依赖说明

- eval-gate / 外部模型实测：需要 `OPENAI_API_KEY` 等凭据，环境未提供时标注"跳过（无 API key）"，不虚标。
- 全量 workspace 门禁仅由 D 在收尾执行；A/B/C 个人只跑自己 crate 范围（结果转抄自各 STATUS，D 已复跑关键项）。

## 遗留问题（不阻塞提交）

- `/traces/{index}`、`/learn/packages/{name}` 等资源型路径空态 404 语义保留（契约明确、前端区分显示）；"traces 空态 200" 留作后续优化。
- 便携 zip 为 ONNX 模型开箱即用载体；NSIS 安装版 tauri.conf.json 未声明 models 资源（不在本轮白名单），模型经 model_dir() 回退链解析。
- `clients/ts/openapi.json` 被 .gitignore 忽略（跟踪异常历史遗留），提交的契约产物为 schema.d.ts 与 dist/；快照随源码仓库外的生成流程再刷新。
- `desktop/web/app.js`：git HEAD 为 UTF-16LE（历史写入遗留），工作区为 B 维护的 UTF-8 完整版（函数清单为超集），提交以工作区 UTF-8 版本为准。
