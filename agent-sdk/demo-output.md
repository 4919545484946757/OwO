# OwO Agent SDK 摘要

**目标：** OwO Agent SDK 是一个 Codex 式的 AI 智能体 SDK，参照 OpenAI Codex / Claude Code 的 Harness 架构，提供 Rust 核心库（Agent loop、工具注册、权限审批、会话审计）、HTTP API（SSE 事件流）和 CLI 交互工具，最终实现可编程、可审批的智能体工作闭环。

**M1 能力：** 已实现最小闭环——Agent loop 驱动模型调用与工具执行，内置五种文件/命令操作工具（读写、列表、搜索、Shell），具备 workspace 作用域校验、deny/ask/allow 权限策略和 CLI 审批机制，支持会话 JSON 持久化、diff 对比与 write 操作 revert 回滚，以及内存级审计记录和 OpenAI 兼容的模型网关。
