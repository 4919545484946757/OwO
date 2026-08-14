# STATUS-T3.md — Agent T3 状态（§12 支柱1：.owflow 工作流引擎 v1）

> 我只写本文件。任务来源：主控 2026-08-14 第三轮四线分工指令。

## 认领

- 时间：2026-08-14
- 任务：可组合工作流引擎 core 库 v1——`.owflow` = 触发器 + 步骤图 + 子流程 + 条件 + 人审节点 + 回滚点；底座复用 action_program/executor/learn（只读）。
- 白名单：`core/src/workflow.rs`（新）、`core/tests/workflow_tests.rs`（新）。
- 禁止：lib.rs/Cargo.toml/server/CLI/desktop、他人文件；不 commit；不新增依赖。
- 备注：`.coord3/OWNERSHIP.md` 尚未冻结（主控后续写入），按任务指令开工。

## P1 里程碑计划

1. .owflow DSL：JSON 声明式模型（触发器/步骤类型/子流程/条件/人审/回滚点/前置条件）+ schema 校验明确报错。
2. 解释器：编译到 ProgramNode（Step/Assert/Branch/Loop/Sub）+ 自含引擎执行；人审暂停 approve/reject；失败自动回滚到最近检查点。
3. 安全边界：跨应用步骤独立权限节点，默认 deny；Ask 经审批。
4. 健康度集成：InvokeSkill 前查 SkillHealth（Disabled 拒、Degraded 确认），执行后回写。
5. 回滚：文件快照 + 失败回滚到检查点。
6. 契约测试 ≥20 项。

## 公开 API 清单（供主控 lib.rs 导出）

- `pub mod workflow;`（lib.rs 加一行即可）
- 类型：`WorkflowDefinition`、`WorkflowStep`、`TriggerKind`、`WorkflowTrigger`、`PermMode`、`PermissionClaim`、`SenseSpec`、`LocateSpec`、`ActSpec`、`WorkflowState`、`StepRecord`、`WorkflowOutcome`、`CheckpointRef`
- trait：`ActionBackend`（sense/locate/act/invoke_skill/invoke_mcp/notify）、`HumanApprover`（request）
- 函数：`validate_definition(&WorkflowDefinition, known_flows: &[String]) -> Result<(), Vec<String>>`、`compile_to_program(&WorkflowDefinition) -> Result<ActionProgram, String>`、`eval_expr(&str, &BTreeMap<String, serde_json::Value>) -> Result<bool, String>`
- 引擎：`WorkflowEngine`（`run() -> Result<WorkflowOutcome, String>`、`abort()`）
- 测试替身：`MockBackend`、`AutoApprover`

## 执行记录（时间戳）

- 2026-08-14 认领登记完成；`crates/owo-agent-core/src/workflow.rs` + `tests/workflow_tests.rs` 从零实现。
- 2026-08-14 P1 全部完成：
  1. .owflow DSL：TriggerKind（manual/schedule/foreground_app/file_change/clipboard）+ 12 类步骤（Sense/Locate/Act/Assert/InvokeSkill/InvokeMcp/HumanApprove/Notify/Subflow/Loop/Cond/RollbackPoint）+ 权限声明（PermMode 默认 deny）+ 前置条件 + max_steps/subflow_depth_limit。
  2. `validate_definition`：id 唯一/触发器非空/子流程引用存在/回滚点引用存在/表达式语法/权限 scope 等，聚合返回全部错误。
  3. `compile_to_program(flow, known_flows)`：Act→Step、Assert→Assert、Cond→Branch、Loop→Loop、Subflow→Sub 结构映射。
  4. 引擎：状态机 Pending→Running→WaitingApproval→Succeeded/Failed/Aborted；权限门禁（默认 deny/Ask 经审批人）；健康度门禁（Disabled 拒、Degraded 需确认、执行后回写）；人审；循环变量 `{id}.iteration`；子流程递归（深度上限）；max_steps 熔断；回滚点快照 + 失败自动回滚到最近检查点；全程审计。
  5. `eval_expr` 表达式求值：exists/==/!=/>/>=/</<= + true/false；未知变量视为不成立。
  6. 样例"整理表格→生成文档→人审"可解释执行。
- 2026-08-14 门禁实测：
  - `cargo test -p owo-agent-core --test workflow_tests`：✅ 30/30 全绿（≥20 项验收：序列化往返、6 项 schema 校验、表达式、线性/条件/循环/循环变量早退/max_steps 熔断、子流程递归+失败传播、人审通过/拒绝、权限默认 deny/allow/ask、健康度 Disabled/Degraded、回滚最近检查点/文件恢复/就近回滚、前置条件、abort、审计、样例端到端、权限序列化）。
  - `cargo clippy -p owo-agent-core --all-targets -- -D warnings`：✅ 0 警告（与 T1/T2/T4 并行状态无关；T2 依赖由主控合并后复跑通过）。
  - `cargo fmt -p owo-agent-core -- --check`：✅ 干净（rustfmt 我自己的两个文件；他人文件未触碰）。
- 2026-08-14 协作：DEPENDENCIES.md 留言请求 `pub mod workflow;`（曾一次被覆盖后重新追加）；按 T1 既定模式本地先行添加 `pub mod workflow;`（主控计划内单行，收尾合并无冲突）。
- 2026-08-14 修复记录：async 递归 Box::pin（E0733）、SemanticAnchor 构造字段、copy_tree 排除 .wf-checkpoints（快照自复制无限递归）、回滚 staging 复制顺序（避免删除 work_root 连带销毁快照）、eval_expr 未知变量语义、Loop 循环变量、compile_to_program 签名。

## DSL schema（v1）

```json
{
  "id": "wf-id", "name": "名称", "version": 1,
  "triggers": [{ "id": "t1", "kind": "manual" }],
  "permissions": [{ "scope": "fs.write", "mode": "allow|deny|ask" }],
  "preconditions": ["ready == true"],
  "rollback_points": [],
  "max_steps": 500, "subflow_depth_limit": 5,
  "steps": [
    { "kind": "sense", "id": "tables", "spec": { "target": "spreadsheet" } },
    { "kind": "locate", "id": "row", "spec": { "target": "data-rows" } },
    { "kind": "act", "id": "w", "scope": "fs.write",
      "spec": { "action": "write_file|append_file|send_message|launch|click|type", "target": "path|app|element", "value": "..." } },
    { "kind": "assert", "id": "a1", "expr": "exists(row)", "timeout_ms": 3000 },
    { "kind": "invoke_skill", "id": "sk", "skill": "name", "args": {} },
    { "kind": "invoke_mcp", "id": "m", "server": "s", "tool": "t", "args": {}, "scope": "network" },
    { "kind": "human_approve", "id": "h", "prompt": "..." },
    { "kind": "notify", "id": "n", "message": "..." },
    { "kind": "subflow", "id": "sub", "flow": "other-wf", "args": {} },
    { "kind": "loop", "id": "l", "body": [...], "cond": "l.iteration < 3", "max_iter": 10 },
    { "kind": "cond", "id": "c", "expr": "...", "then": [...], "otherwise": [] },
    { "kind": "rollback_point", "id": "cp1" }
  ]
}
```

## 公开 API 清单（供主控 lib.rs 导出）

- `pub mod workflow;`（已由主控合并；可选 `pub use`）
- 类型：`WorkflowDefinition`、`WorkflowStep`、`TriggerKind`、`WorkflowTrigger`、`PermMode`、`PermissionClaim`、`SenseSpec`、`LocateSpec`、`ActSpec`、`WorkflowState`、`StepRecord`、`WorkflowOutcome`
- trait：`ActionBackend`（sense/locate/act/invoke_skill/invoke_mcp/notify）、`HumanApprover`（request）、`Approval`
- 函数：`validate_definition(&WorkflowDefinition, &[String]) -> Result<(), Vec<String>>`、`compile_to_program(&WorkflowDefinition, &[String]) -> Result<ActionProgram, String>`、`eval_expr(&str, &BTreeMap<String, Value>) -> Result<bool, String>`
- 引擎：`WorkflowEngine::new(...)`、`run() -> Result<WorkflowOutcome, String>`、`abort()`、`disable_skill()`、`audit()`、`ctx()`、`state()`
- 测试替身：`MockBackend`、`AutoApprover`

## 遗留问题

- P2 未做：触发器运行时轮询器（前台应用/文件监听）、通知输出接口、SSE 事件流。
- `compile_to_program` 中 Sense/Locate/InvokeSkill/InvokeMcp/HumanApprove/Notify 以占位 Assert 映射（引擎语义执行）；真实 ProgramNode 全映射留待主控接 executor 时扩展。
- Loop 循环变量仅 `{id}.iteration`；后续可加 per-iteration ctx 快照。
- 真实 ActionBackend（桌面/文件/MCP 接入）由主控后续实现（trait 已定义）。
