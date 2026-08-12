# OwO 模拟环境（桌面实验台）

后台静默迭代用，不操作真实桌面：

- `owo-sim-qq --headless --port 18500 --log <path>`：自绘 QQ 聊天窗口（离屏 GDI 渲染），
  提供 `/frame`（BMP）、`/ocr`（真值版面）、`/click`、`/type`、`/key`、`/state`、`/log`、`/reset`。
- `owo-sim-browser --port 18201`：本地模拟搜索站（首页/搜索/文章/图片下载）。
- 核心服务设置 `OWO_SIM_QQ_URL=http://127.0.0.1:18500` 后，`screen_ocr / desktop_click / desktop_type`
  等工具自动落到虚拟窗口，不碰真实桌面；HTTP 直连写接口在模拟面下被禁用。

一键验收：

```powershell
$env:OPENAI_API_KEY="sk-..."; $env:OPENAI_BASE_URL="https://api.deepseek.com/v1"
powershell -ExecutionPolicy Bypass -File scripts\run-sim-e2e.ps1
```

验收项：QQ 回复闭环（读上下文→回复→发送→验证→等对方回复→再回复）与
浏览器搜索/浏览/图片下载。
