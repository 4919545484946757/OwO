#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""模拟浏览器站 Agent 端到端验收（后台 headless 版）。

前置：
  1. owo-sim-browser --port 18201
  2. 核心服务以 OWO_SIM_QQ_URL 启动均可；浏览器驱动走 Playwright headless（OWO_BROWSER_HEADLESS=1）

任务：导航本地搜索页 → 输入关键词搜索 → 打开第一篇文章 → 下载文章图片到工作区。
断言：下载文件存在、>0 字节、PNG 头正确。
"""
import argparse
import json
import os
import sys
import time
import urllib.request

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")


def http_json(method, url, body=None, timeout=60):
    data = None if body is None else json.dumps(body, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(url, data=data, method=method)
    request.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.loads(response.read().decode("utf-8"))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default="http://127.0.0.1:4096")
    parser.add_argument("--browser", default="http://127.0.0.1:18201")
    parser.add_argument("--workspace", default=os.getcwd())
    parser.add_argument("--out-path", default="sim/logs/downloads/sim-image-1.png")
    args = parser.parse_args()

    session = http_json(
        "POST",
        args.base + "/session",
        {
            "workspace": args.workspace,
            "model": None,
            "system_prompt": (
                "你是 OwO 浏览器操作 Agent。规则：\n"
                "1. 优先用 browser_navigate / browser_type / browser_press / browser_click "
                "操作页面，browser_snapshot 查看页面状态。\n"
                "2. 下载图片用 browser_download_image，路径必须是工作区内路径。\n"
                "3. 完成下载后用 read_file 或文件工具确认文件存在且非空，再报告。"
            ),
        },
        timeout=30,
    )
    session_id = session["id"]
    prompt = (
        "使用浏览器自动化完成以下任务：\n"
        f"1. browser_navigate 打开 {args.browser}\n"
        "2. 在搜索框（input[name=q]）输入关键词'风景'，然后按 Enter 提交搜索。\n"
        "3. browser_snapshot 查看结果页，用 browser_click 打开第一条结果链接（点击标题文本）。\n"
        "4. 用 browser_download_image 下载文章页里的图片"
        f"（src 选择器 img，或直接使用图片 URL）到 {args.out_path}\n"
        "5. 用 read_file 确认图片文件存在且大小大于 0，报告下载路径、字节数和页面标题。"
    )
    body = json.dumps({"prompt": prompt}, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(
        args.base + "/session/" + session_id + "/turn", data=body, method="POST"
    )
    request.add_header("Content-Type", "application/json")

    tool_uses = []
    final_text = None
    started = time.time()
    with urllib.request.urlopen(request, timeout=1200) as response:
        for raw in response:
            line = raw.decode("utf-8", errors="replace").strip()
            if not line.startswith("data:"):
                continue
            payload = line[5:].strip()
            try:
                event = json.loads(payload)
            except json.JSONDecodeError:
                continue
            event_type = event.get("type")
            if event_type == "tool_use":
                tool_uses.append(event.get("tool"))
                print(
                    "[tool] {} {}".format(
                        event.get("tool"),
                        json.dumps(event.get("args"), ensure_ascii=False)[:160],
                    ),
                    flush=True,
                )
            elif event_type == "tool_result":
                print(
                    "[result] {} ok={} err={}".format(
                        event.get("tool"), event.get("ok"), event.get("error")
                    ),
                    flush=True,
                )
            elif event_type == "final":
                final_text = event.get("text")
                print("[final] {}".format(final_text), flush=True)
    elapsed = time.time() - started

    out_path = os.path.abspath(os.path.join(args.workspace, args.out_path))
    exists = os.path.exists(out_path)
    size = os.path.getsize(out_path) if exists else 0
    png_ok = False
    if exists and size > 0:
        with open(out_path, "rb") as handle:
            png_ok = handle.read(8) == b"\x89PNG\r\n\x1a\n"
    passed = exists and size > 0 and png_ok
    report = {
        "ok": passed,
        "elapsed_s": round(elapsed, 1),
        "tools": sorted(set(tool_uses)),
        "download_path": out_path,
        "download_exists": exists,
        "download_bytes": size,
        "png_header_ok": png_ok,
        "final_text": final_text,
    }
    print(json.dumps(report, ensure_ascii=False, indent=2), flush=True)
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
