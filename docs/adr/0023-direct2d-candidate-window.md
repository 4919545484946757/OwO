# ADR 0023：进程内 Direct2D 候选窗

- 状态：渲染基础已采用；尺寸与单行布局由 ADR 0024 取代
- 日期：2026-08-02

## 背景

现有候选窗是 TSF DLL 内的非激活 Win32 弹窗，依赖 GDI `DrawTextW` 和固定的 640×44 像素尺寸。它能够维持最短输入链路，但中英文字形布局、缩放、候选强调、窗口边缘定位和 Windows 11 视觉效果均不足。设置中心已经使用 WinUI 3，但把 Windows App SDK 引入每个加载 TSF 的宿主进程会扩大 DLL 依赖面、部署风险和输入路径故障域。

## 决策

1. 候选窗继续作为 `OwO.TSF.dll` 创建的进程内 `WS_POPUP`，保持 `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST`；不引入独立 UI 进程，也不把 WinUI 3/Windows App SDK 链接到 TSF。
2. 使用系统 Direct2D HWND Render Target 绘制背景、边框、序号、首候选高亮和文本；使用 DirectWrite 与 `Microsoft YaHei UI` 分别排版输入串、候选和数字标签。GDI 不再承担候选窗正文绘制。
3. 窗口以 DIP 定义 52 高、280～860 宽，根据实际文本测量动态定宽。窗口跨显示器时按 `GetDpiForWindow` 重建像素尺寸；靠近工作区右缘或下缘时水平收敛或翻到光标上方。
4. Windows 11 上请求沉浸式深色、圆角、无系统边框、瞬态系统背景和 DWM 阴影。所有 DWM 属性均为可选增强；系统拒绝某项属性时，Direct2D 深色卡片仍必须可用。
5. Direct2D Factory 和 DirectWrite 格式是设备无关资源；Render Target 与画刷是设备相关资源。`D2DERR_RECREATE_TARGET`、尺寸、DPI、显示器和主题变化触发释放/重建，不能因此中断 TSF 输入状态或候选 IPC。
6. 本变更只替换显示层。按键吞吐、请求代际、分页、数字选择、上屏编辑会话以及 Core Service 协议均不改变。
7. 关闭拼音纠错时，只有每个候选前的数字序号改为橙色；候选窗背景、边框、拼音预览、候选文字、高亮和翻页控件继续使用正常主题色，避免模式提示压过候选内容。

## 结果

候选窗不再固定占用 640 像素，短输入保持紧凑，长候选在设定上限内裁剪；首候选获得明确的视觉层级。依赖仅增加 Windows 自带的 `d2d1.dll`、`dwrite.dll` 和 `dwmapi.dll`，不增加安装包运行时或第三方 UI 组件。TSF DLL 冒烟测试继续验证 COM 加载、类工厂和卸载边界；最终视觉与跨 DPI 行为仍需在 Windows 11 真实应用中手工验收。

## 已知边界

- HWND Render Target 不是逐像素透明的 DirectComposition 表面；瞬态背景由 DWM 是否接受该窗口类型决定，深色卡片本身不依赖透明合成。
- 当前候选仍为单行布局，达到 860 DIP 后裁剪后部候选；翻页和数字选择可继续访问当前页，后续如采用多行必须重新验证光标遮挡和键盘可达性。
- 视觉参数当前没有进入配置 Schema。避免在稳定性验收前让主题设置改变 TSF 资源生命周期。

## 回滚

移除 Direct2D/DirectWrite 资源与 DWM 属性，恢复 `WM_PAINT` 中的 GDI 文本绘制和原链接库即可；候选协议、配置、词典和用户数据均无需迁移。
