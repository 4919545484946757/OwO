# OWNERSHIP.md — 文件归属冻结清单（2026-08-14，Agent D 写定）

> 只准改 OWNERSHIP 里属于自己的文件；需要碰别人文件时，先在 CONTRACT.md 留言等待，不得擅动。
> 本清单开工前冻结，除非四方在 CONTRACT.md 留言一致同意，否则不得变更。

## 文件域（互不重叠）

| 域 | 负责人 | 文件白名单 |
| --- | --- | --- |
| A（HTTP 服务面） | Agent A | `crates/owo-agent-server/src/lib.rs`、`crates/owo-agent-server/Cargo.toml`（仅限 dev 依赖）、`crates/owo-agent-server/tests/route_contract.rs`（新建）、`clients/ts/*`（openapi.json、src/schema.d.ts、构建产物） |
| B（桌面工作台） | Agent B | `desktop/web/index.html`、`desktop/web/app.js`、`desktop/web/style.css` |
| C（回归门禁+打包） | Agent C | `scripts/sim-qq-observe-e2e.py`（必须）、`scripts/sim-regression.py`、`scripts/skill-gate.ps1`、`scripts/package-desktop.ps1`、`scripts/download-onnx-ocr-models.ps1`、`crates/owo-agent-core/src/onnx_ocr.rs`（仅 model_dir() 回退逻辑）、`dist/` 产物、打包所需 `models/ocr` 三件套与 onnxruntime.dll |
| D（文档+协调） | Agent D | `agent-sdk/.coord/*`、`builGoal/技术文档-AI智能体输入法.md`、`agent-sdk/ACCEPTANCE.md`、`agent-sdk/README.md`、根 `AGENTS.md`（仅"技术基线"引用） |

## 共享文件协议

| 文件 | 读写规则 |
| --- | --- |
| `.coord/OWNERSHIP.md` | 只读（除 D 维护）；变更需四方一致 |
| `.coord/CONTRACT.md` | A 发布契约；B 读；任何人留言须带自己的名字前缀；D 处理协调 |
| `.coord/STATUS-A/B/C/D.md` | 各自只写自己的，只读他人的 |
| `.coord/GATES.md` | 仅 D 写 |
| `.coord/COMMIT-PLAN.md` | 仅 D 写 |
| 根 `AGENTS.md` | 仅 D 更新"技术基线"引用，不改行为规则 |

## 冻结时间戳

- 2026-08-14 开工时写定，四方以此为凭开工。
