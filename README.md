# OwO 输入法

OwO 是面向 Windows 11 x64 的模块化中文拼音输入法。基础输入、词典查询和候选上屏不依赖网络；智能排序不可用时会自动使用基础候选。

当前版本：`0.1.0-alpha.2`

## 主要功能

- Windows TSF 输入法，Direct2D/DirectWrite 候选窗口
- 完整拼音、不完整拼音、缩写与可切换纠错
- 本地词频学习和 libime 中文上下文排序
- 候选点击、翻页、展开、滚轮浏览和拼音光标
- WinUI 3 设置中心，可配置候选数量、学习灵敏度和多组快捷键
- ZIP、文件夹和 `.owopkg` 插件安装；高权限插件须单独知情授权

## 安装

推荐运行发行目录中的 `OwO-Input-Method-0.1.0-alpha.2-windows-x64-Setup.exe`。

也可以解压发行 ZIP 后双击 `Install-OwO.cmd`。安装完成后按 `Win + Space`，选择 OwO Input Method。安装、注册和卸载 TSF 时会出现 Windows UAC 提示。

常用入口：

- 设置中心：开始菜单中的“OwO Input Method Settings”，或 `Open-OwO-Settings.cmd`
- 重启服务：`Start-OwO.cmd`
- 卸载：开始菜单卸载项，或 `Uninstall-OwO.cmd`

用户配置、词频和日志位于 `%LOCALAPPDATA%\OwO\InputMethod`。卸载默认保留这些数据。

## 默认操作

- `Space` 或数字键：选择候选
- `[` / `]`：候选翻页
- `Shift+↑` / `Shift+↓`：候选翻页
- `Shift+←` / `Shift+→`：移动拼音光标
- `Ctrl+Space`：切换中英文
- `Ctrl+Q`：切换拼音纠错
- `Enter`：直接输出当前字母

快捷键均可在设置中心启用、关闭或添加多组绑定。

## 构建

需要 Visual Studio 2022 Build Tools、MSVC x64、CMake 3.25+、.NET 10 SDK 和 Windows 11 SDK。

```powershell
cmake --preset windows-debug
cmake --build --preset windows-debug
ctest --preset windows-debug
```

Release 与发行包：

```powershell
cmake --preset windows-release
cmake --build --preset windows-release
.\scripts\build_settings_center.ps1 -Configuration Release
.\scripts\package_release.ps1 -Version 0.1.0-alpha.2
.\scripts\package_setup.ps1 -Version 0.1.0-alpha.2
```

架构与决策记录见 [`docs`](docs)，插件接口见 [`docs/plugins/development.md`](docs/plugins/development.md)。

## 平台与许可

- 当前仅正式支持 Windows 11 x64
- OwO 源码采用 `GPL-3.0-only`
- libime/Fcitx5Utils 采用 `LGPL-2.1-or-later`
- 完整第三方许可随发行包提供于 `LICENSES` 目录

Alpha 构建尚未进行商业代码签名，请只从可信来源获取，并核对发行目录中的 SHA-256 清单。
