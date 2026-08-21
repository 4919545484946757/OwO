# OwO 插件 API v1 开发指南

本文说明 OwO 当前可安装的插件来源、进程协议、标准服务和权限模型。API v1 只支持 Windows 11 上的独立进程插件；插件代码不会加载进 TSF 或 Core Service 进程。

## 1. 可安装来源

设置中心接受三种来源：

- `.owopkg`：推荐的发行格式，本质是受限制的 ZIP 容器。
- `.zip`：使用与 `.owopkg` 完全相同的检查规则，不能包含任意 ZIP 扩展特性。
- 文件夹：适合本地开发；选择后会先将普通文件复制到不可变内存快照，再进入与压缩包相同的清单、签名和授权流程。

所有来源都必须在根目录放置 `manifest.json`。压缩包和文件夹最多 1024 个文件，单文件不超过 64 MiB，未压缩总量不超过 256 MiB。绝对路径、`..`、Windows 设备名、大小写冲突路径、符号链接和重解析点会被拒绝。文件夹安装不会直接从原目录运行，也不会在检查后重新读取已变化的源文件。

未签名文件夹、第三方包以及申请高权限的包不会因为格式放宽而自动受信：设置中心仍要求风险与免责确认、逐包摘要确认和权限授权，安装后保持停用，需由用户再次启用。

## 2. 目录与清单

最小结构：

```text
my-plugin/
├─ manifest.json
├─ config.schema.json
└─ bin/
   └─ my-plugin.exe
```

`manifest.json` 使用封闭的 API v1 字段集合：

```json
{
  "id": "com.example.owo.demo",
  "name": "Demo Plugin",
  "version": "1.0.0",
  "api_version": 1,
  "runtime": "process",
  "entry": "bin/my-plugin.exe",
  "permissions": ["candidate.transform", "config.read"],
  "network": false,
  "config_schema": "config.schema.json"
}
```

`id` 必须是小写反向域名形式，`version` 必须是三段数字版本，`entry` 必须是包内相对 `.exe` 路径。`network` 只有在同时声明 `network.client` 时才可设为 `true`；要求普通 Win32 用户令牌的能力还必须声明 `system.full_trust`。

## 3. 启动契约

OwO 使用以下参数启动插件：

```text
my-plugin.exe --owo-plugin-pipe <pipe> --owo-plugin-id <id> --owo-plugin-data <absolute-data-path>
```

插件只能将持久数据写入 `--owo-plugin-data` 指定的目录。沙箱进程还会收到同值的 `OWO_PLUGIN_DATA`、`TEMP` 和 `TMP`。获准完整信任时额外设置 `OWO_PLUGIN_FULL_TRUST=1`；这个标记不代表管理员权限，也不允许绕过 UAC。

插件连接命名管道后必须先发送 `hello_request`。Core 返回 `hello_response`，当前能力为 `cancel.v1` 和 `invoke.v1`。随后 Core 可发送 `invoke_request`、`cancel_request` 和 `shutdown_request`。插件分别返回 `invoke_response` 或 `acknowledgement`。同一插件同一时刻最多处理一个调用。

公共 C++ 协议定义位于：

- `include/owo/plugin/plugin_protocol.h`
- `include/owo/plugin/plugin_pipe.h`
- `include/owo/plugin/plugin_services.h`
- `include/owo/plugin/plugin_permissions.h`

`apps/example_process_plugin/main.cpp` 是可编译的握手、调用、取消与关闭参考实现。单条 payload 上限为 256 KiB，服务名必须带版本后缀（例如 `.v1`）。自定义接口应使用开发者前缀，例如 `com.example.search.v1`。

## 4. 标准服务

标准服务由 `plugin_services.h` 定义。Core 调用已知服务时会自动补入所需权限，PluginHost 会再次比对当前 manifest 和精确版本授权记录；插件或调用方无法通过漏报权限绕过检查。

| 服务 | 所需权限 | 用途 |
| --- | --- | --- |
| `owo.health.check.v1` | 无 | 检查插件是否就绪 |
| `owo.lifecycle.event.v1` | 无 | 接收启动、停用等生命周期事件 |
| `owo.command.execute.v1` | 无 | 执行插件声明的命令 |
| `owo.dictionary.lookup.v1` | 无 | 对显式查询返回词典结果 |
| `owo.candidate.transform.v1` | `candidate.transform` | 变换 Core 明确传入的一页候选词 |
| `owo.settings.schema.v1` | `config.read` | 返回声明式设置结构 |
| `owo.settings.read.v1` | `config.read` | 读取插件自身设置 |
| `owo.settings.write.v1` | `config.write` | 修改插件自身设置 |
| `owo.notification.show.v1` | `notification.show` | 显示用户通知 |
| `owo.ui.settings-page.v1` | `ui.settings_page` | 提供声明式设置页 |
| `owo.ui.overlay.v1` | `ui.overlay` | 控制已授权的 UI 覆盖层 |
| `owo.ui.desktop-pet.v1` | `ui.desktop_pet` | 控制已授权的桌面宠物 |
| `owo.resource.dictionary.install.v1` | `resource.dictionary.install` | 提供字典资源安装操作 |
| `owo.resource.theme.install.v1` | `resource.theme.install` | 提供主题资源安装操作 |
| `owo.resource.material.install.v1` | `resource.material.install` | 提供材质资源安装操作 |
| `owo.resource.model.install.v1` | `resource.model.install` | 提供排序模型安装操作 |
| `owo.resource.sound.install.v1` | `resource.sound.install` | 提供声音资源安装操作 |

payload 在传输层是不透明 UTF-8 字节串。标准服务建议使用 UTF-8 JSON，并在顶层携带 `schema_version: 1`。插件必须拒绝未知字段或未知 schema；返回失败时使用 `invalid_request`、`permission_denied` 或 `plugin_error`，不要将敏感输入写入 `diagnostic`。

候选变换请求示例：

```json
{
  "schema_version": 1,
  "composition": "nihao",
  "candidates": [
    {"text": "你好", "consumed_syllables": 2},
    {"text": "拟好", "consumed_syllables": 2}
  ]
}
```

响应只返回变换后的显式候选数据，不应请求或推断未提供的窗口文本。Core 保留最终校验、去重、长度限制和上屏控制权。

## 5. 权限目录

除既有剪贴板、输入上下文/提交/替换、文件系统、网络、进程、屏幕、麦克风、桌宠、覆盖层、字典/主题/材质和完整信任权限外，API v1 现提供：

| 权限 | 风险 | 说明 |
| --- | --- | --- |
| `candidate.transform` | 需注意 | 处理 Core 明确传入的候选页 |
| `config.read` | 低 | 读取插件自己的配置 |
| `config.write` | 高 | 修改插件自己的配置 |
| `notification.show` | 需注意、完整信任 | 显示系统通知 |
| `resource.model.install` | 高、完整信任 | 安装排序模型资源 |
| `resource.sound.install` | 高、完整信任 | 安装声音资源 |
| `ui.settings_page` | 需注意 | 提供声明式设置页面 |

任何需要完整信任的权限都必须同时声明 `system.full_trust`。权限在插件升级、manifest 变化、包摘要变化或发布者绑定变化后不会自动继承。TSF 输入线程不能直接调用插件；携带敏感权限的调用只接受 Core 内部标记的可信用户操作来源。

## 6. 构建与调试建议

1. 先复制 `apps/example_process_plugin`，保留握手和取消逻辑。
2. 使用文件夹安装进行本地迭代；每次内容变化都会形成新的清单摘要并重新授权。
3. 只声明实际使用的权限。开发阶段也不要依赖父进程环境变量或当前工作目录。
4. 服务处理必须支持超时与取消；不要在输入线程执行磁盘扫描、联网或模型初始化。
5. 发布时再制作 `.owopkg` 并生成覆盖规范化清单摘要的 `signature.json`。

插件格式的安全边界参见 `docs/security/plugin-baseline.md`，签名与库存摘要格式参见 `docs/adr/0008-plugin-package-and-trust-foundation.md`，授权和运行时隔离分别参见 ADR 0011 至 0017。
