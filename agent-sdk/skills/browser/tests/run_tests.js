// browser 技能契约测试：导航 / 表单 / 截图 + DOM 快照（>=3 个端到端用例）。
// 使用本机 Edge（channel: msedge），无需额外下载浏览器。
"use strict";

const fs = require("fs");
const http = require("http");
const os = require("os");
const path = require("path");
const { chromium } = require("playwright");

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "owo-skill-browser-"));
const pageHtml = `<!doctype html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>OwO 浏览器技能测试</title></head>
<body>
  <h1 id="main">欢迎</h1>
  <form id="login">
    <input id="name">
    <button type="submit" id="go">提交</button>
  </form>
  <div id="result"></div>
  <script>
    document.getElementById("login").addEventListener("submit", (event) => {
      event.preventDefault();
      document.getElementById("result").textContent =
        "你好 " + document.getElementById("name").value;
    });
  </script>
</body>
</html>`;
fs.writeFileSync(path.join(tmp, "index.html"), pageHtml);

function startServer(htmlDir) {
  return new Promise((resolve) => {
    const server = http.createServer((req, res) => {
      if (req.url === "/" || req.url === "/index.html") {
        res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
        res.end(fs.readFileSync(path.join(htmlDir, "index.html")));
      } else {
        res.writeHead(404);
        res.end("not found");
      }
    });
    server.listen(0, "127.0.0.1", () => resolve(server));
  });
}

(async () => {
  const server = await startServer(tmp);
  const base = `http://127.0.0.1:${server.address().port}/`;
  const browser = await chromium.launch({ channel: "msedge", headless: true });
  try {
    const page = await browser.newPage();

    await page.goto(base, { waitUntil: "domcontentloaded" });
    const title = await page.title();
    if (!title.includes("浏览器技能测试")) throw new Error("title 断言失败");
    const heading = await page.textContent("#main");
    if (heading !== "欢迎") throw new Error("h1 断言失败");
    console.log("case1 ok: 导航与断言");

    await page.fill("#name", "OwO");
    await page.click("#go");
    const result = await page.textContent("#result");
    if (result !== "你好 OwO") throw new Error(`表单结果断言失败：${result}`);
    console.log("case2 ok: 表单填写");

    const shot = path.join(tmp, "shot.png");
    await page.screenshot({ path: shot });
    if (!fs.existsSync(shot) || fs.statSync(shot).size === 0) {
      throw new Error("截图为空");
    }
    const snapshot = await page.content();
    if (!snapshot.includes("OwO 浏览器技能测试")) {
      throw new Error("DOM 快照断言失败");
    }
    console.log("case3 ok: 截图与 DOM 快照");
  } finally {
    await browser.close();
    server.close();
    fs.rmSync(tmp, { recursive: true, force: true });
  }
})().catch((error) => {
  console.error("browser skill gate failed:", error);
  process.exit(1);
});
