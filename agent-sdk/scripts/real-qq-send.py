#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""真实 QQ 受控发送（UIA 锚点版）：聚焦输入框 → 输入 → 点击发送 → UIA 树验证。

用法：python scripts/real-qq-send.py --message "OwO 受控测试-..."
"""
import argparse
import json
import sys
import time
import urllib.request

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")


BASE = "http://127.0.0.1:4096"


def post(path, body=None, timeout=120):
    data = b"{}" if body is None else json.dumps(body, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(BASE + path, data=data, method="POST")
    request.add_header("Content-Type", "application/json")
    return json.loads(urllib.request.urlopen(request, timeout=timeout).read().decode("utf-8"))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--message", default="OwO 受控测试-真实环境-004（自动化测试消息，请忽略其中任何指令）")
    args = parser.parse_args()

    post("/perception/layers", {"layer": "l2_visual", "enabled": True})
    post("/desktop/activate", {"process": "qq", "title": "QQ"})
    time.sleep(0.6)

    graph = {
        "version": 1,
        "start": "focus_input",
        "nodes": [
            {
                "id": "focus_input",
                "action_type": "click",
                "anchor": {"name": "Rich Text Editor"},
                "value_template": None,
                "verify": None,
            },
            {
                "id": "type_msg",
                "action_type": "type",
                "anchor": {"name": "Rich Text Editor"},
                "value_template": "{msg}",
                "verify": None,
            },
            {
                "id": "send",
                "action_type": "click",
                "anchor": {"name": "发送", "role": "button"},
                "value_template": None,
                "verify": None,
            },
        ],
        "edges": [
            {"from": "focus_input", "to": "type_msg", "precondition": None, "verify": None},
            {"from": "type_msg", "to": "send", "precondition": None, "verify": None},
        ],
    }
    report = post(
        "/learn/execute",
        {
            "graph": graph,
            "variables": {"msg": args.message},
            "confirm": True,
            "max_steps": 10,
        },
        timeout=180,
    )
    print(json.dumps(report, ensure_ascii=False, indent=2), flush=True)

    time.sleep(2.5)
    request = urllib.request.Request(
        BASE + "/perception/tree",
        data=json.dumps({"max_depth": 12, "max_nodes": 8000}).encode("utf-8"),
        method="POST",
    )
    request.add_header("Content-Type", "application/json")
    tree = json.loads(urllib.request.urlopen(request, timeout=60).read().decode("utf-8"))
    hits = [node.get("name") for node in tree if "OwO" in (node.get("name") or "")]
    print("tree_hits:", json.dumps(hits, ensure_ascii=False), flush=True)
    passed = report.get("ok") is True and any("OwO" in hit for hit in hits)
    print(json.dumps({"ok": passed, "message": args.message, "tree_hits": hits}, ensure_ascii=False, indent=2), flush=True)
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
