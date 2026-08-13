#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""真实网页浏览器 Agent 端到端验收（headless，不弹窗、不干扰桌面）。

任务：Bing 搜索 → 打开第一条结果 → 下载页面图片到工作区。
断言：下载文件存在且非空（PNG/JPEG/WebP/SVG 任一）。
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


def preflight(base):
    try:
        health = http_json("GET", base + "/health", None, timeout=10)
    except Exception as exc:
        print(f"[preflight] 无法连接服务 {base}: {exc}", flush=True)
        sys.exit(2)
    if not health.get("auto_approve"):
        print(
            f"[preflight] {base} 未开启 OWO_AUTO_APPROVE=1：多轮工具调用会在审批处挂起 300s。"
            "模拟面请以 OWO_AUTO_APPROVE=1 重启服务。",
            flush=True,
        )
        sys.exit(2)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default="http://127.0.0.1:4096")
    parser.add_argument("--workspace", default=os.getcwd())
    parser.add_argument("--out-path", default="sim/logs/downloads/web-image-1")
    parser.add_argument("--query", default="rust programming language")
    args = parser.parse_args()

    preflight(args.base)

    session = http_json(
        "POST",
        args.base + "/session",
        {
            "workspace": args.workspace,
            "model": None,
            "system_prompt": (
                "你是 OwO 浏览器操作 Agent。规则：\n"
                "1. 用 browser_navigate / browser_snapshot / browser_click / browser_type / browser_press 操作页面。\n"
                "2. 下载图片用 browser_download_image，路径必须是工作区内路径；"
                "src 可以是 CSS 选择器，也可以直接用 url。\n"
                "3. 页面若加载慢，用 browser_snapshot 重试，不要反复导航。\n"
                "4. 完成下载后用 run_command 检查文件大小（二进制文件不要用 read_file）。"
            ),
        },
        timeout=30,
    )
    session_id = session["id"]
    query = args.query
    out_path = args.out_path
    prompt = (
        "用浏览器自动化完成以下任务：\n"
        f"1. browser_navigate 打开 https://www.bing.com/search?q={urllib.request.quote(query)}\n"
        "2. browser_snapshot 查看结果页，找第一条看起来是官方/权威的结果链接并 browser_click 打开。\n"
        "3. browser_snapshot 查看打开的页面，找到页面里的任意一张图片。\n"
        f"4. 用 browser_download_image 下载该图片（src 选择器 img 或直接 url）到 {out_path}\n"
        "5. 用 run_command 确认文件存在且大小大于 0，报告下载路径、字节数、页面标题。"
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

    out_abs = os.path.abspath(os.path.join(args.workspace, out_path))
    candidates = [out_abs]
    # 模型可能自动补了扩展名（.png/.jpg/.svg 等）
    base = out_abs
    for suffix in (".png", ".jpg", ".jpeg", ".webp", ".svg", ".gif"):
        candidates.append(base + suffix)
    found = [path for path in candidates if os.path.exists(path) and os.path.getsize(path) > 0]
    size = os.path.getsize(found[0]) if found else 0
    passed = bool(found)
    report = {
        "ok": passed,
        "elapsed_s": round(elapsed, 1),
        "tools": sorted(set(tool_uses)),
        "download_paths": found,
        "download_bytes": size,
        "final_text": final_text,
    }
    print(json.dumps(report, ensure_ascii=False, indent=2), flush=True)
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
