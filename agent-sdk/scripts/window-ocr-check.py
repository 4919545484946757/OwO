#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""窗口级 OCR 检查：抓取指定窗口，检查底部输入区与关键词，结果写 UTF-8 报告文件。

用法：python scripts/window-ocr-check.py --hwnd 198064 --out <path>
"""
import argparse
import json
import sys
import urllib.request

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--hwnd", type=int, required=True)
    parser.add_argument("--base", default="http://127.0.0.1:4097")
    parser.add_argument("--out", default=None)
    args = parser.parse_args()

    request = urllib.request.Request(
        args.base + "/perception/window",
        data=json.dumps({"hwnd": args.hwnd}).encode("utf-8"),
        method="POST",
    )
    request.add_header("Content-Type", "application/json")
    result = json.loads(urllib.request.urlopen(request, timeout=300).read().decode("utf-8"))
    text = result.get("text", "")
    lines = result.get("lines", [])
    bottom = [line for line in lines if line.get("y", 0) > 700]
    report = {
        "hwnd": args.hwnd,
        "provider": result.get("provider"),
        "chars": result.get("chars"),
        "lines": len(lines),
        "has_send": "发送" in text.replace(" ", ""),
        "has_input_msg": "输入消息" in text.replace(" ", ""),
        "has_search": "搜索" in text.replace(" ", ""),
        "bottom_lines": bottom,
    }
    payload = json.dumps(report, ensure_ascii=False, indent=2)
    print(payload, flush=True)
    if args.out:
        with open(args.out, "w", encoding="utf-8") as handle:
            handle.write(payload)


if __name__ == "__main__":
    sys.exit(main())
