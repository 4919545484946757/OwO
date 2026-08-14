# AGENTS.md

本仓库包含两个项目：

- `OwO 输入法`（根目录 C++ 工程）：历史基线，当前不主动修改。
- `agent-sdk/`：Codex 式 Agent 智能体 SDK，**当前活跃开发目标**。

## 开发规则

- 所有新开发放在 `agent-sdk/` 内；不修改 OwO C++ 输入法代码。
- 技术基线：`builGoal/技术文档-AI智能体输入法.md`（v0.6，只实施 Agent 智能体方案，输入法路线不实施）。
- 模型凭据只经环境变量（`OPENAI_API_KEY` 等），禁止写入代码、配置或提交。
- 权限默认 deny；任何工具调用必须经过权限策略，审批与主 Agent 分离。
- M1 验收项：会话、审计、diff/revert、工具权限必须保持工作，改动需带契约测试。
- Rust 代码保持 `cargo fmt` 与 `clippy` 干净；契约测试随功能提交。
- **文件编码**：所有源文件必须为 UTF-8；Windows 下写入含中文的 .rs/.md 文件时禁止经 GBK 控制台中转（会导致 mojibake 损坏）。提交前用 `cargo fmt --check` + `git diff` 抽查。
- **并行协作**：多个 Agent 并行时按 `AGENTS-COORD.md` 认领文件；同一文件同一时间只允许一个 Agent 修改；涉及 `owo-agent-server/src/lib.rs` 等核心文件的改动需先跑 `cargo check` 验证。
- **HTTP 契约**：服务端新增/修改路由必须同步 `tests/route_contract_tests.rs`（路由面契约测试），防止接口回归丢失。
