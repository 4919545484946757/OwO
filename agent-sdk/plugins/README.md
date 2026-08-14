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
## 签名与市场治理（M4b）

三个示例插件已带 Ed25519 签名（manifest 内 `signature` 字段）与 `versions.json`
（版本 → App 最低版本映射，当前 `1.0.0 → 0.5.0`）。

- 校验方：core 库 `plugin::verify_plugin_signature`（摘要口径 = sha256(id|name|version|entry[|入口内容])）。
- 签名工具：`scripts/plugin-sign.ps1`（底层 `plugin-sign.py`，Ed25519）：

  ```powershell
  .\scripts\plugin-sign.ps1 generate -KeyFile "$env:TEMP\owo-plugin-key.pem"
  .\scripts\plugin-sign.ps1 sign -PluginDir .\plugins\owo-translate -KeyFile "$env:TEMP\owo-plugin-key.pem"
  .\scripts\plugin-sign.ps1 verify -PluginDir .\plugins\owo-translate
  ```

- 安全约定：私钥永不入库（生成到 $env:TEMP 或用户私有目录）；示例插件使用一次性测试密钥，
  正式分发应由发布者持私有密钥签名，公钥随包分发。
- 静态扫描：入口脚本危险 API（os.system/subprocess/eval…）与域外网络请求会被
  `plugin::scan_plugin_for_risks` 拦截；联网插件需在 manifest 声明 `network_allowlist`。
