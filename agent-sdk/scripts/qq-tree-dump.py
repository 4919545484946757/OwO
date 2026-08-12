#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""真实 QQ UIA 树调试：激活 QQ → 可选 Ctrl+F 搜索 → 打印匹配关键字节点。

用法：python scripts/qq-tree-dump.py --search "26大创" --keyword "26"
"""
import argparse
import json
import sys
import time
import urllib.request

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")


BASE = "http://127.0.0.1:4097"


def post(path, body=None, timeout=60):
    data = b"{}" if body is None else json.dumps(body, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(BASE + path, data=data, method="POST")
    request.add_header("Content-Type", "application/json")
    return json.loads(urllib.request.urlopen(request, timeout=timeout).read().decode("utf-8"))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--search", default=None)
    parser.add_argument("--keyword", default="")
    parser.add_argument("--max-nodes", type=int, default=10000)
    args = parser.parse_args()

    post("/desktop/activate", {"process": "qq", "title": "QQ"})
    time.sleep(0.6)
    if args.search:
        post("/desktop/shortcut", {"combo": "ctrl+f"})
        time.sleep(0.8)
        post("/desktop/type", {"text": args.search})
        time.sleep(1.2)
    tree = post("/perception/tree", {"max_depth": 14, "max_nodes": args.max_nodes})
    print("nodes:", len(tree), flush=True)
    for node in tree:
        name = node.get("name") or ""
        if not args.keyword or args.keyword in name:
            print(
                "name={!r} rect=({},{},{},{})".format(
                    name,
                    node.get("x"),
                    node.get("y"),
                    node.get("width"),
                    node.get("height"),
                ),
                flush=True,
            )


if __name__ == "__main__":
    sys.exit(main())
