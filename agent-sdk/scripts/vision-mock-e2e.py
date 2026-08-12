#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""BYOK 视觉通道端到端验证（mock OpenAI-compatible 端点，不依赖真实视觉模型）。

前置：核心服务以 OWO_VISION_PROVIDER=openai、OWO_VISION_BASE_URL=http://127.0.0.1:18300/v1、
OWO_VISION_MODEL=mock-vl 启动；owo-sim-qq --headless 在 18500。

流程：重置模拟 → 发送一条消息 → /vision/describe（mock 返回固定描述）→
/vision/verify 两个 yes/no 问题 → 断言解析结果与请求中确实携带图片。
"""
import argparse
import json
import sys
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")


received = {"count": 0, "has_image": False, "image_bytes": 0}


class MockVisionHandler(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(length)
        try:
            value = json.loads(body.decode("utf-8"))
        except Exception:
            value = {}
        received["count"] += 1
        prompt = json.dumps(value, ensure_ascii=False)
        received["has_image"] = received["has_image"] or "data:image/png;base64," in prompt
        if received["has_image"] and "data:image/png;base64," in prompt:
            received["image_bytes"] = len(
                prompt.split("data:image/png;base64,", 1)[1].split('"', 1)[0]
            )
        if "输入框是否已清空" in prompt:
            content = "YES (confidence 0.95)：输入框已清空"
        elif "出现了一条新消息" in prompt:
            content = "YES (confidence 0.93)：聊天区出现新消息"
        elif "描述" in prompt:
            content = "这是一个模拟 QQ 聊天窗口，左侧有联系人列表，右侧是聊天区，底部有输入框和发送按钮。"
        else:
            content = "NO (confidence 0.6)：无法确定"
        response = json.dumps(
            {"choices": [{"message": {"role": "assistant", "content": content}}]}
        ).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(response)))
        self.end_headers()
        self.wfile.write(response)

    def log_message(self, *args):
        pass


def http_json(method, url, body=None, timeout=120):
    data = None if body is None else json.dumps(body, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(url, data=data, method=method)
    request.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.loads(response.read().decode("utf-8"))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default="http://127.0.0.1:4096")
    parser.add_argument("--sim", default="http://127.0.0.1:18500")
    parser.add_argument("--mock-port", default=18300)
    args = parser.parse_args()

    server = ThreadingHTTPServer(("127.0.0.1", args.mock_port), MockVisionHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()

    try:
        http_json("POST", args.sim + "/reset", {}, timeout=10)
        time.sleep(1.5)
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
        http_json("POST", args.sim + "/type", {"text": "视觉通道测试-001"}, timeout=10)
        http_json("POST", args.sim + "/key", {"key": "enter"}, timeout=10)
        time.sleep(1)

        describe = http_json(
            "POST",
            args.base + "/vision/describe",
            {"prompt": "请用中文描述这个界面。"},
            timeout=120,
        )
        verify_input = http_json(
            "POST",
            args.base + "/vision/verify",
            {"question": "输入框是否已清空？"},
            timeout=120,
        )
        verify_message = http_json(
            "POST",
            args.base + "/vision/verify",
            {"question": "聊天记录区域是否出现了一条新消息？"},
            timeout=120,
        )

        print("[describe]", describe.get("description"), flush=True)
        print("[verify input]", verify_input.get("answer"), verify_input.get("confidence"), flush=True)
        print("[verify message]", verify_message.get("answer"), verify_message.get("confidence"), flush=True)
        print("[mock received] calls={} has_image={} image_b64_bytes={}".format(
            received["count"], received["has_image"], received["image_bytes"]
        ), flush=True)

        passed = (
            "聊天窗口" in describe.get("description", "")
            and verify_input.get("answer") == "yes"
            and verify_message.get("answer") == "yes"
            and received["has_image"]
            and received["image_bytes"] > 10000
        )
        report = {
            "ok": passed,
            "provider": describe.get("provider"),
            "model": describe.get("model"),
            "description": describe.get("description"),
            "input_cleared": verify_input.get("answer"),
            "message_visible": verify_message.get("answer"),
            "mock_calls": received["count"],
            "mock_received_image": received["has_image"],
            "image_b64_bytes": received["image_bytes"],
        }
        print(json.dumps(report, ensure_ascii=False, indent=2), flush=True)
        return 0 if passed else 1
    finally:
        server.shutdown()


if __name__ == "__main__":
    sys.exit(main())
