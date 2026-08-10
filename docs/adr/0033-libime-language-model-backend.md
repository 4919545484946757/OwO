# ADR 0033：采用 fcitx/libime 语言模型后端

- 状态：已接受
- 日期：2026-08-09

## 背景

OwO 已有 `BigramModel`、本地用户词频和 ModelHost 排序协议，但基础 Bigram 只有内存测试实现，缺少可发布的静态中文 N-gram 模型。项目决定采用 `fcitx/libime`，替代许可证和产品适配性不足的旧 Bigram 示例项目。

## 决策

1. 固定使用 libime 1.1.15；官方源码归档 SHA-256 为 `e8ce7b90035aeafa5ce5f59a05f84d6c192fcecc009b2e74cf179bc18b21eaf5`。
2. libime 及其项目内语言模型资源按 `LGPL-2.1-or-later` 管理，发布包必须携带许可证、组件版本、来源和修改说明。
3. libime 不进入 `OwO.TSF.dll`。Windows 首版通过独立 ModelHost 后端使用 libime，并复用现有有超时、取消和降级能力的版本化 IPC。
4. Core 的基础候选生成和当前 `BigramModel` 接口继续保留。libime 不可用、模型缺失、响应超时或返回非法候选时，候选顺序保持基础结果，不阻塞输入。
5. libime 的静态语言模型负责通用 N-gram 上下文排序；OwO 已有的本地词频与会话上下文学习继续负责用户自适应，两者分开存储并可独立关闭。
6. libime 官方 C++ ABI 不直接暴露为 OwO 公共 SDK。后端桥接采用 OwO 自有协议，以便升级或移除 libime 时不破坏 Core、TSF 和插件接口。
7. 源码、语言模型和词典分别固定版本与哈希。不得在普通构建或测试中隐式联网下载大型模型。

## 中文模型资源

首个 Windows x64 模型固定为 Fcitx 官方 `macos-latest` 发布中的架构无关数据资产；标签名称只表示打包流水线，`zh_CN.lm` 本身不包含平台机器码：

- GitHub Release ID：`364819099`；
- Asset ID：`501261436`；
- 资产更新时间：`2026-08-04T11:29:17Z`；
- `chinese-addons-any.tar.bz2` SHA-256：`e3158a51ab3026bca7823d89e99e27c30d8a51e723f300b516caf5d94525b139`；
- `lib/libime/zh_CN.lm` SHA-256：`3588b3942c8fd62e1a6bd3bae8c7cadc0faf928c175b245224ed475862218387`；
- 模型文件大小：34,736,327 字节。

普通 CMake 配置和测试不会联网获取该模型。开发环境通过 `scripts/fetch_libime_model.ps1` 显式下载、断点续传、校验并只提取 `zh_CN.lm`。

## Windows 集成约束

libime 1.1.15 的官方构建依赖 Fcitx5Utils 5.1.20、Boost iostreams、Zstd、ECM，并在 Core 内构建 KenLM。OwO 主工程使用 MSVC；若可用的 libime Windows 构建采用不同 C++ ABI，则后端必须保持进程隔离，不能把该 DLL 直接链接进 MSVC Core。

首个接入切片只实现候选重排，不替换 OwO 的拼音解析、词典、Beam Search、候选 UI 或用户数据格式。

## 许可证与分发

OwO 主项目为 GPLv3，libime 的 `LGPL-2.1-or-later` 组件可以作为独立依赖分发。发行物仍须：

- 附带 libime 的 LGPL-2.1-or-later 许可证文本及版权声明；
- 标明使用的准确版本、源码获取地址和 OwO 对其所作修改；
- 保留允许用户替换或重新构建 LGPL 组件的方式；
- 对 Boost、Zstd、Fcitx5Utils、KenLM 及模型数据生成独立依赖清单，不能用 libime 的总许可证替代其各自声明。

## 回滚

关闭 libime 后端启动选项并停止分发对应后端与模型即可。Core、TSF、基础候选和本地学习路径不依赖 libime，现有配置与用户词频无需迁移。
