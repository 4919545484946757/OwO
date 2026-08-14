# COMMIT-PLAN.md — 提交方案（Agent D 产出，2026-08-14）

> 本轮全程无人私自 commit；主控已先行落库两个中间提交：`a593b29`（服务面恢复+文档纪律基线）、`91ba9b2`（.coord 协作状态记录）。本方案覆盖**剩余差异**，按 A/B/C/D 四组逻辑提交，主控可依序直接执行（`git add` 按组 + 提交即可）。

## 前置说明（提交前必读）

1. **app.js 编码**：git HEAD 版本为 UTF-16LE（历史 PowerShell 写入遗留），工作区为 B 维护的 UTF-8 完整版（函数清单为超集，含 friendlyError resource 模式 + computer-use 契约字段 + 409 幂等提示）。提交后 git diff 会显示全文件变更——**以工作区 UTF-8 版本为准**，不要用 HEAD 覆盖。提交时若 git 提示行尾转换，保持工作区内容不动。
2. **openapi.json 被 gitignore**：`clients/ts/.gitignore:4` 忽略 `openapi.json`（历史遗留跟踪异常），无法也不应提交；契约产物以 `src/schema.d.ts`（重新生成）与 `dist/` 为准。
3. **dist/ 被 gitignore**（根 `.gitignore:5`）：打包产物（zip/NSIS/latest.json）仅存在于磁盘，不入库；验收证据在 STATUS-C/GATES。
4. **route_contract_tests.rs 白名单 1 行修复**：由 D 经主控授权补 `/perception/template/{app_id}`（资源型 404 语义），归入 A 组提交并说明。
5. 每条提交的验证证据均已实测，对应 `.coord/GATES.md` 门禁矩阵（294 项全绿等）。

## 提交组 A：HTTP 服务面恢复 + 路由契约测试（Agent A）

```
fix(server): 补登 OpenAPI 24 路径 + 路由面契约测试全量覆盖
```

- 文件：
  - `agent-sdk/crates/owo-agent-server/src/lib.rs`（openapi_spec 补登 /desktop/*、/vision/*、/perception/template/*、/perception/elements、/perception/ocr/bytes、/perception/window、/learn/status、/openapi.json）
  - `agent-sdk/crates/owo-agent-server/tests/route_contract_tests.rs`（契约快照全路径非 404/405 + /openapi.json 覆盖断言 + 真实 HTTP smoke；资源型 404 白名单 8 项含 D 授权补的 /perception/template/{app_id}）
  - `agent-sdk/crates/owo-agent-server/Cargo.toml`（dev-deps：tower 0.5 util + tempfile 3）
  - `agent-sdk/clients/ts/src/schema.d.ts`（openapi.json 重新生成）
  - `agent-sdk/Cargo.lock`
- 验证：`cargo test -p owo-agent-server`（route_contract 3/3）✅；`cargo test --workspace` 294 项 ✅；`cargo fmt/clippy` ✅；`npm run typecheck/build/test:unit` ✅（A STATUS + D 复跑）。

## 提交组 B：桌面工作台联调（Agent B）

```
feat(desktop): 面板错误处理统一 friendlyError + P2 computer-use 任务面板
```

- 文件：`agent-sdk/desktop/web/app.js`（UTF-8 工作区版本；18+10 处 catch 友好错误、资源型 404 区分、computer-use 契约字段对齐、409 幂等提示）
- 验证：`node --check` 0 错误 ✅；33 项接口矩阵 ALL PASS ✅（B STATUS）；`GET /` 静态托管 200 ✅。

## 提交组 C：回归门禁修复 + ONNX 打包（Agent C）

```
fix(core): onnx_ocr model_dir 回退链（env→数据目录→exe 同级→仓库路径）
```

- 文件（主体已随主控中间提交 `a593b29` 入库，本组为状态收尾）：
  - `agent-sdk/crates/owo-agent-core/src/onnx_ocr.rs`（model_dir() 回退 + 3 单测）——已在 a593b29
  - `agent-sdk/scripts/sim-qq-observe-e2e.py`（seen_kinds 只统计字符串 detail.type）——已在 a593b29
  - 本轮无新增代码差异；`dist/` 产物（gitignore 不入库，时间戳 2026-08-14）
- 验证：sim-regression 2/2 ✅；skill-gate 12/12 ✅；解包自检三项 ✅（C STATUS + GATES#7/8/11）。

## 提交组 D：文档与验收基线收敛（Agent D）

```
docs: ACCEPTANCE 章节编号收敛（一~五十二）+ 技术文档 C.2/C.3 校准 v0.6 基线
```

- 文件：
  - `agent-sdk/ACCEPTANCE.md`（章节编号修复：51 个编号标题按文件顺序重排，消除重复"四十八"与乱序；新增"五十二、v0.5.8 交付面收敛"节，如实记录四 Agent 交付面）
  - `builGoal/技术文档-AI智能体输入法.md`（C.2 补记 M4 前奏骨架 + TS SDK 行；C.3 实测数字校准：294 项、路由契约 3/3、eval-gate 标跳过原因）
  - `AGENTS.md`（技术基线 v0.3 → v0.6）——已在 a593b29，无需重复
  - `agent-sdk/README.md`（头部基线 v0.6 + 当前基线小节）——已在 a593b29
- 验证：ACCEPTANCE 52 节编号 sequential=True、无重复（脚本断言）✅；测试数 294 与 GATES 一致 ✅。

## 提交组 COORD：协作状态收尾（Agent D 维护，可并入 D 组或独立提交）

```
docs(coord): 契约发布 + 门禁矩阵全绿 + 提交方案（2026-08-14）
```

- 文件：`agent-sdk/.coord/CONTRACT.md`（新增）、`agent-sdk/.coord/GATES.md`（最终矩阵）、`agent-sdk/.coord/STATUS-A.md`、`agent-sdk/.coord/STATUS-B.md`、`agent-sdk/.coord/STATUS-C.md`、`agent-sdk/.coord/STATUS-D.md`（各自收尾）
- 建议：与 D 组合并为一个 docs 提交，或独立 `docs(coord)` 提交，二者皆可。

## 执行顺序（主控）

1. `git add agent-sdk/crates/owo-agent-server agent-sdk/clients/ts/src/schema.d.ts agent-sdk/Cargo.lock` → 提交 A
2. `git add agent-sdk/desktop/web/app.js` → 提交 B
3. 提交 C 无剩余差异（已入库），跳过或仅状态文件
4. `git add agent-sdk/ACCEPTANCE.md builGoal/技术文档-AI智能体输入法.md` → 提交 D
5. `git add agent-sdk/.coord` → 提交 COORD
6. 复核：`git status` 干净；`git log` 无未计划提交。

## 禁止事项

- 不执行 `git commit`（由主控统一提交）；不 amend/reset/force-push；不把 openapi.json、dist/、凭据加入提交。
