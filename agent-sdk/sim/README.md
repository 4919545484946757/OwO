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

扩展场景：

- 多联系人切换：`sim/scenarios/qq-multi-contact.json`（张子豪 + 李四），
  提示词 `sim/prompts/qq-multi-contact.txt`；e2e 用
  `--prompt-file sim\prompts\qq-multi-contact.txt --require-contacts-file sim\prompts\qq-multi-contacts.txt`
  断言两条会话都发出过消息。
- 真实网页（headless）：`scripts/web-browser-e2e.py`（Bing/360 搜索 → 打开结果 → 下载页面图片；
  网络受限时 Agent 会自动换可用站点）。

操作记忆闭环（M-D 起步）：

```powershell
& python scripts\sim-qq-learn-e2e.py --base http://127.0.0.1:4096 --sim http://127.0.0.1:18500 --value "技能复用验证-001"
```

脚本化示范一次 QQ 回复 → 自动录制动作（内容掩码）→ 泛化出 `{value}` 变量 → 沉淀 `qq_reply` 技能包 →
重置场景后换参数复用执行并断言发送成功。
