# AGENTS.md

本仓库包含两个项目：

- `OwO 输入法`（根目录 C++ 工程）：历史基线，当前不主动修改。
- `agent-sdk/`：Codex 式 Agent 智能体 SDK，**当前活跃开发目标**。

## 开发规则

- 所有新开发放在 `agent-sdk/` 内；不修改 OwO C++ 输入法代码。
- 技术基线：`builGoal/技术文档-AI智能体输入法.md`（v0.3，只实施 Agent 智能体方案，输入法路线不实施）。
- 模型凭据只经环境变量（`OPENAI_API_KEY` 等），禁止写入代码、配置或提交。
- 权限默认 deny；任何工具调用必须经过权限策略，审批与主 Agent 分离。
- M1 验收项：会话、审计、diff/revert、工具权限必须保持工作，改动需带契约测试。
- Rust 代码保持 `cargo fmt` 与 `clippy` 干净；契约测试随功能提交。
