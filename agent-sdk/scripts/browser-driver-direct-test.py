#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""浏览器驱动直连测试（不经 Agent）：真实网页搜索 → 打开结果 → 下载图片。

用途：隔离“驱动/网络”与“模型循环”问题；headless 模式不依赖交互桌面。
"""
import json
import os
import subprocess
import sys
import time

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")


def main():
    node = os.environ.get(
        "OWO_BROWSER_NODE",
        r"C:\Users\23843\.cache\codex-runtimes\codex-primary-runtime\dependencies\node\bin\node.exe",
    )
    node_modules = os.environ.get(
        "OWO_BROWSER_NODE_PATH",
        r"C:\Users\23843\.cache\codex-runtimes\codex-primary-runtime\dependencies\node\node_modules",
    )
    script = os.path.join(os.path.dirname(os.path.abspath(__file__)), "browser-driver.js")
    env = dict(os.environ)
    env["NODE_PATH"] = node_modules
    env["OWO_BROWSER_HEADLESS"] = "1"
    proc = subprocess.Popen(
        [node, script],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        env=env,
    )

    next_id = 1

    def call(command, args):
        nonlocal next_id
        request = {"id": next_id, "cmd": command, "args": args}
        next_id += 1
        proc.stdin.write(json.dumps(request, ensure_ascii=False) + "\n")
        proc.stdin.flush()
        deadline = time.time() + 180
        while time.time() < deadline:
            line = proc.stdout.readline()
            if not line:
                raise RuntimeError("驱动进程退出")
            response = json.loads(line)
            if response.get("id") == request["id"]:
                if response.get("ok"):
                    return response.get("data")
                raise RuntimeError(response.get("error"))
        raise RuntimeError("命令超时: " + command)

    out_path = os.path.abspath(os.path.join(os.getcwd(), "sim", "logs", "downloads", "web-direct"))
    try:
        nav = call("navigate", {"url": "https://www.so.com/s?q=rust+programming+language"})
        print("[navigate]", nav.get("title"), nav.get("url"), flush=True)
        snapshot = call("snapshot", {})
        print("[snapshot] links:", len(snapshot.get("links", [])), "images:", len(snapshot.get("images", [])), flush=True)
        links = [
            link
            for link in snapshot.get("links", [])
            if "so.com/s" not in link.get("href", "")
            and len(link.get("text", "")) > 4
        ]
        rusty = [link for link in links if "rust" in link.get("text", "").lower()]
        links = rusty or links
        if not links:
            raise RuntimeError("搜索结果未找到 rust 链接")
        downloaded = False
        for target in links[:4]:
            print("[open]", target.get("text"), target.get("href"), flush=True)
            try:
                call("navigate", {"url": target["href"]})
            except Exception as error:
                print("[open-failed]", error, flush=True)
                continue
            page = call("snapshot", {})
            print("[page]", page.get("title"), "images:", len(page.get("images", [])), flush=True)
            if not page.get("images"):
                continue
            dl = call("download_image", {"src": "img", "path": out_path})
            downloaded = True
            break
        if not downloaded:
            raise RuntimeError("前 4 个结果页均无可下载图片")
        print("[download]", json.dumps(dl, ensure_ascii=False), flush=True)
        exists = os.path.exists(out_path) and os.path.getsize(out_path) > 0
        print(json.dumps({"ok": exists, "path": out_path, "bytes": os.path.getsize(out_path) if exists else 0}, ensure_ascii=False), flush=True)
        return 0 if exists else 1
    finally:
        try:
            proc.stdin.write(json.dumps({"id": 999, "cmd": "close", "args": {}}) + "\n")
            proc.stdin.flush()
        except Exception:
            pass
        try:
            proc.wait(timeout=10)
        except Exception:
            proc.kill()


if __name__ == "__main__":
    sys.exit(main())
