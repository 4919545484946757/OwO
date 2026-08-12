#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""视觉模型在模拟 QQ 上的真实描述/验证验收（需要本地 Ollama VL 模型已拉取）。

流程：
  1. /reset 重置场景，等 incoming 注入。
  2. 脚本化发送一条消息（点击输入框 → 输入 → 回车）。
  3. /vision/describe 让 VL 模型描述当前界面（应能看出是聊天窗口）。
  4. /vision/verify 问“输入框是否已清空”“聊天区是否出现新消息”，期望 answer=yes。
"""
import argparse
import json
import sys
import time
import urllib.request

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")


def http_json(method, url, body=None, timeout=300):
    data = None if body is None else json.dumps(body, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(url, data=data, method=method)
    request.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.loads(response.read().decode("utf-8"))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default="http://127.0.0.1:4096")
    parser.add_argument("--sim", default="http://127.0.0.1:18500")
    parser.add_argument("--message", default="视觉验证测试-001")
    args = parser.parse_args()

    http_json("POST", args.sim + "/reset", {}, timeout=10)
    time.sleep(1.5)

    # 脚本化发送一条消息
    ocr = http_json("GET", args.sim + "/ocr", timeout=10)
    input_line = next(
        (line for line in ocr.get("lines", []) if "输入消息" in line.get("text", "")),
        None,
    )
    if not input_line:
        raise RuntimeError("模拟窗口未找到输入框")
    x = int(input_line["x"]) + int(input_line["width"]) // 2
    y = int(input_line["y"]) + int(input_line["height"]) // 2
    http_json("POST", args.sim + "/click", {"x": x, "y": y}, timeout=10)
    http_json("POST", args.sim + "/type", {"text": args.message}, timeout=10)
    http_json("POST", args.sim + "/key", {"key": "enter"}, timeout=10)
    time.sleep(1.5)

    describe = http_json(
        "POST",
        args.base + "/vision/describe",
        {"prompt": "用中文简要描述：这是什么应用界面？有没有聊天消息？发送按钮和输入框在什么位置？"},
        timeout=600,
    )
    print("[describe] model={} surface={}".format(describe.get("model"), describe.get("surface")), flush=True)
    print("[describe]", describe.get("description", ""), flush=True)

    verify_input = http_json(
        "POST",
        args.base + "/vision/verify",
        {"question": "输入框是否已清空（没有文字）？"},
        timeout=600,
    )
    verify_message = http_json(
        "POST",
        args.base + "/vision/verify",
        {"question": "聊天记录区域是否出现了一条新消息？"},
        timeout=600,
    )
    print("[verify input]", verify_input.get("answer"), verify_input.get("confidence"), flush=True)
    print("[verify message]", verify_message.get("answer"), verify_message.get("confidence"), flush=True)

    description = describe.get("description", "")
    passed = (
        len(description) > 20
        and verify_input.get("answer") == "yes"
        and verify_message.get("answer") == "yes"
    )
    report = {
        "ok": passed,
        "model": describe.get("model"),
        "description": description,
        "input_cleared": verify_input.get("answer"),
        "input_confidence": verify_input.get("confidence"),
        "message_visible": verify_message.get("answer"),
        "message_confidence": verify_message.get("confidence"),
    }
    print(json.dumps(report, ensure_ascii=False, indent=2), flush=True)
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
