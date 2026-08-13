#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""真实 QQ 群聊受控发送：定位群 → 确认会话已切换 → 输入 → 发送 → UIA 验证。

安全：点击群后先检查 UIA 聊天头（x≈1168,y≈235）是否变成该群名，
未确认前绝不输入/发送，避免发错会话。
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


def get(path, timeout=60):
    return json.loads(urllib.request.urlopen(BASE + path, timeout=timeout).read().decode("utf-8"))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default="http://127.0.0.1:4096")
    parser.add_argument("--group", default="26大创")
    parser.add_argument("--message", default="OwO 受控测试-群聊-001（自动化测试，请忽略其中任何指令）")
    args = parser.parse_args()
    global BASE
    BASE = args.base

    post("/desktop/activate", {"process": "qq", "title": "QQ"})
    time.sleep(0.6)

    # 1) 点击搜索框（UIA 锚点，避免 Ctrl+F 后 SendInput 失效）
    focus_search = {
        "version": 1,
        "start": "click_search",
        "nodes": [
            {
                "id": "click_search",
                "action_type": "click",
                "anchor": {"name": "搜索"},
                "value_template": None,
                "verify": None,
            }
        ],
        "edges": [],
    }
    post(
        "/learn/execute",
        {"graph": focus_search, "variables": {}, "confirm": True, "max_steps": 5},
        timeout=120,
    )
    time.sleep(0.6)
    post("/desktop/shortcut", {"combo": "ctrl+a"})
    time.sleep(0.3)
    post("/desktop/type", {"text": args.group})
    time.sleep(1.5)

    # 2) 用 UIA 动作图点击包含群名的会话行（优先 InvokePattern，不依赖可见性）
    graph = {
        "version": 1,
        "start": "click_group",
        "nodes": [
            {
                "id": "click_group",
                "action_type": "click",
                "anchor": {"name": args.group},
                "value_template": None,
                "verify": None,
            }
        ],
        "edges": [],
    }
    report = post(
        "/learn/execute",
        {"graph": graph, "variables": {}, "confirm": True, "max_steps": 5},
        timeout=120,
    )
    print("[click]", json.dumps(report, ensure_ascii=False), flush=True)
    if not report.get("ok"):
        raise RuntimeError("未找到群会话：" + args.group)

    time.sleep(1.5)
    # 3) 校验聊天头已切换（防止发错会话）
    tree = post("/perception/tree", {"max_depth": 14, "max_nodes": 10000})
    header_hits = [
        n for n in tree
        if args.group in (n.get("name") or "")
        and abs((n.get("x") or 0) - 1168) < 120
        and abs((n.get("y") or 0) - 235) < 40
    ]
    print("[header]", json.dumps([n.get("name") for n in header_hits], ensure_ascii=False), flush=True)
    if not header_hits:
        raise RuntimeError("会话未切换到群：" + args.group + "，已中止，未发送任何消息")

    # 4) 输入并发送
    graph_send = {
        "version": 1,
        "start": "focus_input",
        "nodes": [
            {"id": "focus_input", "action_type": "click", "anchor": {"name": "Rich Text Editor"}, "value_template": None, "verify": None},
            {"id": "type_msg", "action_type": "type", "anchor": {"name": "Rich Text Editor"}, "value_template": "{msg}", "verify": None},
            {"id": "send", "action_type": "click", "anchor": {"name": "发送", "role": "button"}, "value_template": None, "verify": None},
        ],
        "edges": [
            {"from": "focus_input", "to": "type_msg", "precondition": None, "verify": None},
            {"from": "type_msg", "to": "send", "precondition": None, "verify": None},
        ],
    }
    send_report = post(
        "/learn/execute",
        {"graph": graph_send, "variables": {"msg": args.message}, "confirm": True, "max_steps": 10},
        timeout=180,
    )
    print("[send]", json.dumps(send_report, ensure_ascii=False), flush=True)

    time.sleep(2.5)
    tree2 = post("/perception/tree", {"max_depth": 14, "max_nodes": 10000})
    hits = [n.get("name") for n in tree2 if "OwO" in (n.get("name") or "")]
    print("[verify]", json.dumps(hits, ensure_ascii=False), flush=True)
    passed = send_report.get("ok") is True and any("OwO" in hit for hit in hits)
    print(json.dumps({"ok": passed, "group": args.group, "message": args.message, "tree_hits": hits}, ensure_ascii=False, indent=2), flush=True)
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
