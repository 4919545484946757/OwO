# 官方示例插件

随包附带两个官方示例插件（技术文档 P0：“插件 SDK 随包附带 2 个官方示例插件”）：

| 插件 | id | 工具 | 权限 |
|---|---|---|---|
| 翻译 | `owo.plugin.translate` | `translate`（演示词典，中英常用短语，无网络依赖） | `agent:tools` |
| 剪贴板历史 | `owo.plugin.clipboard` | `clipboard_read` / `clipboard_write`（Windows 经 PowerShell） | `agent:tools`、`clipboard:read`、`clipboard:write` |

## 运行要求

- 需要 `python` 在 PATH 中（示例服务器为 Python 标准库实现，无第三方依赖）。
- manifest 中 `mcp.args` 使用相对 SDK 根目录的路径（`plugins/<name>/server.py`），
  因此请从 `agent-sdk/` 目录启动 `owo-agent serve`（`scripts/start-dev-service.ps1` 默认如此）。
- 服务启动时自动发现并连接工作区 `plugins/` 下的插件，工具以
  `owo-translate_translate`、`owo-clipboard_clipboard_read` 等命名注册进 Agent。

## 剪贴板权限说明

`clipboard:read` 为只读感知；`clipboard:write` 属注入类操作，需权限策略放行
（默认 deny，符合“一切能力可声明、可审批”）。
