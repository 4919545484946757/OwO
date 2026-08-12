#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""双轮模型冒烟：无工具的连续两轮对话，验证多轮请求链路。"""
import argparse
import json
import os
import sys
import time
import urllib.request

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")


def post(base, path, body=None, timeout=60):
    data = b"{}" if body is None else json.dumps(body, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(base + path, data=data, method="POST")
    request.add_header("Content-Type", "application/json")
    return json.loads(urllib.request.urlopen(request, timeout=timeout).read().decode("utf-8"))


def run_turn(base, session_id, prompt, timeout=120):
    request = urllib.request.Request(
        base + "/session/" + session_id + "/turn",
        data=json.dumps({"prompt": prompt}).encode("utf-8"),
        method="POST",
    )
    request.add_header("Content-Type", "application/json")
    started = time.time()
    final = None
    with urllib.request.urlopen(request, timeout=timeout) as response:
        for raw in response:
            line = raw.decode("utf-8", errors="replace").strip()
            if not line.startswith("data:"):
                continue
            try:
                event = json.loads(line[5:].strip())
            except json.JSONDecodeError:
                continue
            if event.get("type") == "final":
                final = event.get("text")
    return round(time.time() - started, 1), final


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default="http://127.0.0.1:4099")
    args = parser.parse_args()
    session = post(args.base, "/session", {"workspace": os.getcwd(), "model": None})
    session_id = session["id"]
    for index, prompt in enumerate(["1+1=?", "2+2=?，请只回答数字"], start=1):
        elapsed, final = run_turn(args.base, session_id, prompt)
        print(f"turn{index}: {elapsed}s final={final!r}", flush=True)
        if final is None:
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
