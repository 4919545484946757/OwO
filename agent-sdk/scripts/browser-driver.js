// OwO Agent 浏览器驱动：stdin/stdout JSONL 协议，Playwright + 本机 Edge（持久化 profile）。
// 命令：navigate / search / snapshot / click / type / press / screenshot / download_image / close
// 请求：{"id":1,"cmd":"navigate","args":{...}}；响应：{"id":1,"ok":true,"data":{...}}
// 使用 OWO_BROWSER_PROFILE 指定用户数据目录，保持登录态；OWO_SKILL_RUNTIME 提供 node_modules。
"use strict";

const fs = require("fs");
const os = require("os");
const path = require("path");
const readline = require("readline");
const { chromium } = require("playwright");

const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
let context = null;
let page = null;
const profileDir =
  process.env.OWO_BROWSER_PROFILE || path.join(os.tmpdir(), "owo-agent-browser-profile");

async function ensureBrowser() {
  if (context) return;
  fs.mkdirSync(path.dirname(profileDir), { recursive: true });
  context = await chromium.launchPersistentContext(profileDir, {
    channel: "msedge",
    headless:
      process.env.OWO_BROWSER_HEADLESS === "1" ||
      process.env.OWO_BROWSER_HEADLESS === "true",
    viewport: { width: 1280, height: 860 },
    acceptDownloads: true,
    args: ["--disable-blink-features=AutomationControlled"],
  });
  page = context.pages()[0] || (await context.newPage());
}

function respond(id, ok, data, error) {
  process.stdout.write(JSON.stringify({ id, ok, data, error }) + "\n");
}

async function handle(command, args, id) {
  try {
    await ensureBrowser();
    let data;
    switch (command) {
      case "navigate": {
        await page.goto(args.url, {
          waitUntil: args.wait_until || "domcontentloaded",
          timeout: 60000,
        });
        data = { url: page.url(), title: await page.title() };
        break;
      }
      case "search": {
        const query = String(args.query || "");
        const engine = args.engine === "baidu" ? "baidu" : "bing";
        const url =
          engine === "baidu"
            ? `https://www.baidu.com/s?wd=${encodeURIComponent(query)}`
            : `https://www.bing.com/search?q=${encodeURIComponent(query)}`;
        await page.goto(url, { waitUntil: "domcontentloaded", timeout: 60000 });
        await page.waitForTimeout(1800);
        const results = await page.evaluate(() => {
          const out = [];
          const seen = new Set();
          const nodes = document.querySelectorAll(
            "li.b_algo h2 a, h3 a, h2 a, .c-container h3 a, .result h2 a"
          );
          for (const anchor of nodes) {
            const title = (anchor.textContent || "").trim();
            const href = anchor.href || "";
            if (!title || !href.startsWith("http") || seen.has(href)) continue;
            seen.add(href);
            let snippet = "";
            const block = anchor.closest("li, .c-container, .result");
            if (block) {
              const node = block.querySelector(".b_caption p, .c-span-last, p");
              if (node) snippet = (node.textContent || "").trim().slice(0, 300);
            }
            out.push({ title, url: href, snippet });
            if (out.length >= 10) break;
          }
          return out;
        });
        data = {
          query,
          engine,
          url: page.url(),
          title: await page.title(),
          results,
        };
        break;
      }
      case "snapshot": {
        const maxItems = Number(args.max_items || 60);
        data = await page.evaluate((maxItems) => {
          const links = Array.from(document.querySelectorAll("a"))
            .map((anchor) => ({
              text: (anchor.textContent || "").trim().slice(0, 80),
              href: anchor.href || "",
            }))
            .filter((item) => item.text && item.href.startsWith("http"))
            .slice(0, maxItems);
          const images = Array.from(document.querySelectorAll("img"))
            .map((img) => ({ src: img.src || "", alt: img.alt || "" }))
            .slice(0, 40);
          const inputs = Array.from(document.querySelectorAll("input, textarea, select"))
            .map((element) => ({
              tag: element.tagName.toLowerCase(),
              id: element.id || "",
              name: element.name || "",
              placeholder: element.placeholder || "",
              type: element.type || "",
            }))
            .slice(0, 20);
          const text = (document.body ? document.body.innerText : "").slice(0, 6000);
          return {
            url: location.href,
            title: document.title,
            text,
            links,
            images,
            inputs,
          };
        }, maxItems);
        break;
      }
      case "click": {
        if (args.text) {
          const locator = page.getByText(String(args.text), { exact: !!args.exact }).first();
          await locator.click({ timeout: 10000 });
        } else if (args.selector) {
          await page.click(String(args.selector), { timeout: 10000 });
        } else {
          throw new Error("click 需要 selector 或 text");
        }
        data = { ok: true, url: page.url() };
        break;
      }
      case "type": {
        if (args.selector) {
          await page.fill(String(args.selector), String(args.text || ""));
        } else {
          await page.keyboard.type(String(args.text || ""), { delay: Number(args.delay || 10) });
        }
        data = { ok: true };
        break;
      }
      case "press": {
        await page.keyboard.press(String(args.key || ""));
        data = { ok: true };
        break;
      }
      case "screenshot": {
        const abs = path.resolve(String(args.path));
        fs.mkdirSync(path.dirname(abs), { recursive: true });
        await page.screenshot({ path: abs, fullPage: !!args.full_page });
        data = { path: abs, bytes: fs.statSync(abs).size };
        break;
      }
      case "download_image": {
        let url = String(args.url || "");
        if (args.src) {
          const attr = await page
            .getAttribute(String(args.src), "src")
            .catch(() => "");
          if (attr) url = String(attr);
        }
        if (url && !/^[a-z][a-z0-9+.-]*:\/\//i.test(url)) {
          url = new URL(url, page.url()).href;
        }
        if (!url) throw new Error("未指定图片 URL 或 src 选择器");
        const abs = path.resolve(String(args.path));
        fs.mkdirSync(path.dirname(abs), { recursive: true });
        const response = await page.request.get(url, { timeout: 60000 });
        if (!response.ok()) throw new Error(`图片下载失败 HTTP ${response.status()}`);
        const buffer = await response.body();
        fs.writeFileSync(abs, buffer);
        data = {
          path: abs,
          bytes: buffer.length,
          content_type: response.headers()["content-type"] || "",
        };
        break;
      }
      case "close": {
        await context.close();
        context = null;
        page = null;
        data = { ok: true };
        break;
      }
      default:
        throw new Error("未知命令: " + command);
    }
    respond(id, true, data, null);
  } catch (error) {
    respond(id, false, null, String((error && error.message) || error));
  }
}

rl.on("line", (line) => {
  if (!line.trim()) return;
  let request;
  try {
    request = JSON.parse(line);
  } catch {
    return;
  }
  handle(request.cmd, request.args || {}, request.id);
});

rl.on("close", () => {
  if (context) {
    context
      .close()
      .catch(() => {})
      .finally(() => process.exit(0));
  } else {
    process.exit(0);
  }
});
