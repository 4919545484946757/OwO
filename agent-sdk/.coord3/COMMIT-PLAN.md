# COMMIT-PLAN.md — 提交方案（主控产出，2026-08-14）

> 覆盖前两轮遗留 + 第三轮 T1~T4 全部工作。按逻辑组拆分，每组一次提交；
> 全部门禁已实测通过（见 GATES.md），可顺序直接执行。

## 提交 1：core 云端执行正式化（A1/T1 前身 + 第三轮 M4a 延续）

```
feat(core): cloud_exec v0.2 传输抽象/任务队列/进度事件/CLI cloud 子命令
```

- 文件：`core/src/cloud_exec.rs`、`core/tests/cloud_exec_tests.rs`、`cli/main.rs`
- 证据：cloud_exec_tests 21/21；`owo-agent cloud` 全子命令实测（submit/list/diff/apply/revert）

## 提交 2：多格式笔记 v1（T1）

```
feat(core): notes 多格式文档模型 v1（块树/doc.json/MD 往返/HTML 消毒/画布/FTS/零丢失）
```

- 文件：`core/src/notes.rs`、`core/tests/notes_tests.rs`、`core/src/lib.rs`（pub mod notes）
- 证据：notes_tests 27/27

## 提交 3：插件市场治理（T2）

```
feat(core): 插件市场治理骨架（Ed25519 签名/静态扫描/versions 兼容/安装更新回滚/审计）
```

- 文件：`core/src/plugin.rs`、`core/tests/plugin_lifecycle_tests.rs`、`plugins/example-hello/manifest.json`、`plugins/owo-translate/manifest.json`、`plugins/owo-clipboard/manifest.json`、`plugins/README.md`、`scripts/plugin-sign.py`、`scripts/plugin-sign.ps1`（新增）
- 证据：plugin_lifecycle_tests 17/17；plugin-sign 对示例插件 sign→verify 跑通

## 提交 4：工作流引擎（T3）

```
feat(core): .owflow 可组合工作流引擎 v1（DSL/解释执行/权限/健康度/回滚/子流程）
```

- 文件：`core/src/workflow.rs`、`core/tests/workflow_tests.rs`、`core/src/lib.rs`（pub mod workflow）
- 证据：workflow_tests 30/30（含主控修复的 rollback 快照位置）

## 提交 5：Goal/Plan 编排（T4）

```
feat(core): Goal/Plan 多 Agent 编排 v1（DAG 调度/并行限流/重试/replan/恢复/预算）
```

- 文件：`core/src/goal.rs`、`core/src/plan.rs`、`core/tests/goal_plan_tests.rs`、`core/src/lib.rs`（pub mod goal/plan）
- 证据：goal_plan_tests 21/21

## 提交 6：computer-use 审批闭环（前轮 A4 + 第三轮延续）

```
feat(core): computer-use 审批版闭环（动作门禁/敏感熔断/感知闭环/超时预算）+ 任务字段扩展
```

- 文件：`core/src/computer_use.rs`、`core/src/computer_task.rs`、`core/tests/computer_use_tests.rs`
- 证据：computer_use_tests 11/11

## 提交 7：HTTP 服务面契约（前轮 A + 主控合并）

```
fix(server): OpenAPI 补 24 路径登记 + route_contract 全量契约测试 + /computer-use/* 接线
```

- 文件：`server/src/lib.rs`、`server/tests/route_contract_tests.rs`、`server/Cargo.toml`（tower/tempfile dev-deps）、`clients/ts/src/schema.d.ts`
- 证据：route_contract_tests 3/3；/openapi.json 106 路径与路由一致；npm typecheck/build/test:unit 通过

## 提交 8：桌面工作台联调（前轮 B）

```
feat(desktop): 面板 friendlyError 统一 + computer-use 契约字段 + 409 提示
```

- 文件：`desktop/web/app.js`
- 证据：node --check 0 错误；接口矩阵非 404

## 提交 9：依赖合并（主控）

```
build(core): 新增 ed25519-dalek/sha2（签名校验）+ server dev-deps tower/tempfile
```

- 文件：`Cargo.toml`、`core/Cargo.toml`、`Cargo.lock`
- 证据：workspace 全量编译/测试全绿

## 提交 10：文档与协作记录（前轮 D + 主控）

```
docs(coord): 三轮验收记录（ACCEPTANCE/GATES/STATUS/TECH）与技术文档 v0.6 同步
```

- 文件：`ACCEPTANCE.md`、`builGoal/技术文档-AI智能体输入法.md`、`.coord/*`、`.coord2/*`、`.coord3/*`、`plugins/README.md`（README 并入提交 3 或 10，按 git add 粒度定）
- 证据：GATES.md 全绿矩阵

## 执行顺序说明

1. 提交 1-6（core）→ 提交 7（server）→ 提交 8（desktop）→ 提交 9（依赖）→ 提交 10（docs）。
2. 依赖提交 9 放 7 之后是因为 Cargo.lock 同时被 7 引用（dev-deps 与正式依赖都在 lock 中，先 core 后 server 无冲突）。
3. 全部门禁已由主控在提交前实测（GATES.md），提交后无需重跑。
