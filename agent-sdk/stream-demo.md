# OwO Agent SDK 摘要

OwO Agent SDK 是基于 Rust 的 Codex 式 AI 智能体框架（v0.1），提供 Agent loop、工具注册、权限审批、会话与审计等核心能力。内置文件读写（带快照）、目录搜索、命令执行等工具，支持 build/plan 双模式，提供 TUI 与 REPL 交互终端。写操作需经审批，支持 diff 查看与 undo 回滚。模型凭据经环境变量注入，兼容 OpenAI API。
