# GATES.md — 第三轮收尾门禁矩阵（主控维护，2026-08-14 实测）

> 状态约定：✅ 通过 / ❌ 失败 / ⏭ 跳过及原因。全量门禁由主控 2026-08-14 收尾统一执行。

## 全量门禁（2026-08-14 主控实测）

| # | 门禁 | 结果 | 实测摘要 | 证据 |
| --- | --- | --- | --- | --- |
| 1 | Rust fmt | ✅ 通过 | `cargo fmt --all -- --check` 干净（主控统一格式化含 T1~T4 文件） | 主控实测 |
| 2 | Rust clippy | ✅ 通过 | `cargo clippy --workspace --all-targets -- -D warnings` 0 警告（主控修 5 处：goal.rs 未用 enum/借用逃逸、notes.rs 引用、plugin.rs unwrap、workflow.rs 参数/引用） | 主控实测 |
| 3 | Rust test | ✅ 通过 | `cargo test --workspace` 全绿：lib 238 + audit_search 7 + cloud_exec 21 + computer_use 11 + eval 3 + goal_plan 21 + loop 20 + mcp 13 + memory_health 6 + notes 27 + plugin_lifecycle 17 + scene_locate 6 + workflow 30 + server route_contract 3 + server 其他 3 + cli 7 ≈ **393+ 项**（较上轮 323 增加 70+） | 主控实测 |
| 4 | T1 notes | ✅ 通过 | `notes_tests` 27/27（块树/持久化/MD 往返/HTML 消毒/画布/FTS/零丢失/100 份样例） | 主控实测 |
| 5 | T2 plugin | ✅ 通过 | `plugin_lifecycle_tests` 17/17（签名/versions/扫描/安装回滚/审计；依赖 ed25519-dalek+sha2 已由主控合并） | 主控实测 |
| 6 | T3 workflow | ✅ 通过 | `workflow_tests` 30/30（DSL/解释执行/权限/健康度/回滚/子流程/循环；主控修复 rollback 快照目录位置 bug） | 主控实测 |
| 7 | T4 goal/plan | ✅ 通过 | `goal_plan_tests` 21/21（DAG/并行限流/重试/replan/恢复幂等/abort/预算/审计；主控修复 WorkerRegistry 借用） | 主控实测 |
| 8 | 前轮回归 | ✅ 通过 | cloud_exec 21/21、computer_use 11/11、route_contract 3/3（含 106 路径覆盖）、TS typecheck/build/test:unit（前轮已验） | 主控实测 |
| 9 | 依赖合并 | ✅ 通过 | `ed25519-dalek = "2"`、`sha2 = "0.10"`（workspace + core）；去重 T2 误加的重复行；tower/tempfile 保持 server dev-deps | 主控实测 |
| 10 | 编码纪律 | ✅ 通过 | workflow_tests.rs 编码损坏（GBK 双重编码 19 行）已恢复；后续所有文件扫描无损坏 | 主控实测 |
| 11 | 外部依赖项 | ⏭ 跳过 | eval-gate 需 OPENAI_API_KEY 真实凭据，环境已提供但本轮不跑真实模型 eval（属外部验收，C.4 保持"开放"） | — |

## 修复记录（主控收尾介入）

- goal.rs：`StepError` 未用 enum 删除；`run_step_attempts` 借用逃逸 → WorkerRegistry 按值传递（内部 Arc 克隆）
- workflow.rs：`snapshots_root` 从 work_root 内部移到外部（rollback 删除 work_root 会连带删快照 → 回滚后工作区变空的根因）；未用参数清理
- plugin.rs：`is_some()` + `unwrap()` → `if let`
- notes.rs：clippy 引用/格式修复
- workflow_tests.rs：19 行 GBK 双重编码恢复（写入者后续已自行覆盖为干净版本，最终编译/测试全绿）

## 遗留

- 提交尚未执行（见 COMMIT-PLAN.md）；前两轮遗留 25+ 文件一并纳入提交方案。
