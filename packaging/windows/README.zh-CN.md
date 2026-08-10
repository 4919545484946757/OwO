# OwO 输入法 0.1.0-alpha.1

首个面向 Windows 11 x64 的预览发行版。包含 TSF 输入法、Core Service、完整拼音词库、
本地词频学习、libime 中文上下文排序、候选窗口和 WinUI 3 设置中心。

## 安装

1. 解压整个 ZIP，不能只打开压缩包内的文件。
2. 双击 `Install-OwO.cmd`。
3. 安装完成后按 `Win + Space`，选择 `OwO Input Method (P1 Prototype)`。

安装范围仅限当前 Windows 用户；首次注册和卸载 TSF 时会显示 Windows UAC 管理员授权提示。
运行文件安装到
`%LOCALAPPDATA%\Programs\OwO\InputMethod\0.1.0-alpha.1`，用户配置、词频和日志保存在
`%LOCALAPPDATA%\OwO\InputMethod`。

## 使用与卸载

- `Start-OwO.cmd`：重新启动后台运行环境。
- `Open-OwO-Settings.cmd`：直接打开 WinUI 3 设置中心。
- 开始菜单中的“OwO 输入法设置”：打开设置中心。
- `Uninstall-OwO.cmd`：注销输入法并移除自动启动项，默认保留用户数据。

这是未进行商业代码签名的 Alpha 构建，Windows 可能显示来源警告。不要从不可信来源下载
重新封装的版本；可用 `SHA256SUMS.txt` 核对发行文件。
