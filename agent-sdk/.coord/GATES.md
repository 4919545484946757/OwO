# GATES.md — 收尾门禁矩阵（Agent D 维护）

> 状态约定：`⏳ 待跑` / `✅ 通过` / `❌ 失败` / `⏭️ 跳过（原因）`。D 在收尾统一执行；A/B/C 结果以各自 STATUS 为准转抄并注明证据。

## 收尾门禁矩阵（2026-08-14）

| # | 门禁 | 命令 | 结果 | 实测输出摘要 | 证据 |
| --- | --- | --- | --- | --- | --- |
| 1 | Rust fmt | `cargo fmt --all -- --check` | ⏳ 待跑 | | |
| 2 | Rust clippy | `cargo clippy --workspace --all-targets -- -D warnings` | ⏳ 待跑 | | |
| 3 | Rust test | `cargo test --workspace` | ⏳ 待跑 | | |
| 4 | A 路由契约测试 | `cargo test -p owo-agent-server`（含 route_contract） | ⏳ 待跑（以 STATUS-A 为准） | | |
| 5 | node 语法 | `node --check desktop/web/app.js` | ⏳ 待跑 | | |
| 6 | Python 语法 | `python -m py_compile`（本轮改动的 py 脚本） | ⏳ 待跑 | | |
| 7 | sim 回归 | `python scripts/sim-regression.py`（需拉起 sim/agent 服务） | ⏳ 待跑（以 STATUS-C 为准） | | |
| 8 | 技能门禁 | `powershell scripts/skill-gate.ps1` | ⏳ 待跑（以 STATUS-C 为准） | | |
| 9 | 契约一致性 | `/openapi.json` 与路由一致（A 自证 + curl 矩阵非 404） | ⏳ 待跑（以 STATUS-A 为准） | | |
| 10 | 前端面板矩阵 | 面板接口非 404 矩阵（B 自证） | ⏳ 待跑（以 STATUS-B 为准） | | |
| 11 | 打包自检 | 解包便携包 /health 200 + onnx_models_present=true + OCR provider=onnx-v4 | ⏳ 待跑（以 STATUS-C 为准） | | |
| 12 | TS SDK | `npm run typecheck / build / test:unit`（clients/ts） | ⏳ 待跑（以 STATUS-A 为准） | | |
| 13 | 外部依赖门禁 | eval-gate（需 API key）等 | ⏳ 待跑→标跳过原因 | | |

## 依赖顺序与外部依赖说明

- eval-gate / 外部模型实测：需要 `OPENAI_API_KEY` 等凭据，环境未提供时标注"跳过（无 API key）"，不虚标。
- 全量 workspace 门禁仅由 D 在收尾执行；个人只跑自己 crate 范围。
