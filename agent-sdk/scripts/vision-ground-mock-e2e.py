#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""视觉 grounding 交叉验证 e2e（mock OpenAI-compatible 端点）。

前置：核心服务以 OWO_VISION_PROVIDER=openai、OWO_VISION_BASE_URL=http://127.0.0.1:18301/v1 启动；
owo-sim-qq --headless 在 18500。

断言：
  1. 描述“发送按钮” → mock 给正确框 → 与模拟真值版面重合 → matched=true、line=发送。
  2. 描述“不存在的按钮” → mock 给 NONE → matched=false。
  3. 描述“输入框” → mock 给错误框 → 与 OCR 不重合 → matched=false（交叉验证门控生效）。
"""
import argparse
import json
import sys
import threading
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")


class MockGroundHandler(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(length)
        prompt = body.decode("utf-8", errors="replace")
        if "发送按钮" in prompt:
            content = "BOX 815,624,170,36"
        elif "不存在的按钮" in prompt:
            content = "NONE"
        elif "输入框" in prompt:
            content = "BOX 0,0,100,100"
        else:
            content = "BOX 10,10,50,50"
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
    parser.add_argument("--mock-port", default=18301)
    args = parser.parse_args()

    server = ThreadingHTTPServer(("127.0.0.1", args.mock_port), MockGroundHandler)
    threading.Thread(target=server.serve_forever, daemon=True).start()

    try:
        http_json("POST", args.sim + "/reset", {}, timeout=10)
        hit = http_json(
            "POST",
            args.base + "/vision/ground",
            {"description": "发送按钮"},
            timeout=120,
        )
        missing = http_json(
            "POST",
            args.base + "/vision/ground",
            {"description": "不存在的按钮"},
            timeout=120,
        )
        mismatch = http_json(
            "POST",
            args.base + "/vision/ground",
            {"description": "输入框"},
            timeout=120,
        )
        print("[hit]", json.dumps(hit, ensure_ascii=False), flush=True)
        print("[missing]", json.dumps(missing, ensure_ascii=False), flush=True)
        print("[mismatch]", json.dumps(mismatch, ensure_ascii=False), flush=True)

        passed = (
            hit.get("matched") is True
            and "发送" in hit.get("line", {}).get("text", "")
            and hit.get("cross_validated") is True
            and missing.get("matched") is False
            and mismatch.get("matched") is False
            and "未重合" in mismatch.get("reason", "")
        )
        report = {
            "ok": passed,
            "hit": hit,
            "missing": missing,
            "mismatch": mismatch,
        }
        print(json.dumps(report, ensure_ascii=False, indent=2), flush=True)
        return 0 if passed else 1
    finally:
        server.shutdown()


if __name__ == "__main__":
    sys.exit(main())
