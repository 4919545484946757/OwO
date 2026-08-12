#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""真实 QQ 受控回复验收：定位“张子豪-室友”会话 → 输入 → 发送 → OCR 验证。

前置：本机 QQ 已登录并运行；核心服务真实桌面模式（4096）。
安全：消息带“OwO 受控测试”标记，发送前打印将点击的坐标与消息内容。
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


def post(path, body=None, timeout=60):
    data = b"{}" if body is None else json.dumps(body, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(BASE + path, data=data, method="POST")
    request.add_header("Content-Type", "application/json")
    return json.loads(urllib.request.urlopen(request, timeout=timeout).read().decode("utf-8"))


def ocr_lines(x0, y0, x1, y1):
    result = post("/perception/ocr", timeout=120)
    boxes = [
        b
        for b in result.get("boxes", [])
        if x0 <= b["x"] <= x1 and y0 <= b["y"] <= y1
    ]
    boxes.sort(key=lambda b: (b["y"], b["x"]))
    lines = []
    for b in boxes:
        yc = b["y"] + b["height"] // 2
        placed = False
        for line in lines:
            lc = line["y"] + line["height"] // 2
            if (
                abs(lc - yc) <= max(8, line["height"] // 2)
                and b["x"] >= line["x"]
                and b["x"] - (line["x"] + line["width"]) <= 60
            ):
                line["text"] += b["text"]
                line["width"] = (b["x"] + b["width"]) - line["x"]
                line["height"] = max(line["height"], b["height"])
                placed = True
                break
        if not placed:
            lines.append(
                {
                    "text": b["text"],
                    "x": b["x"],
                    "y": b["y"],
                    "width": b["width"],
                    "height": b["height"],
                }
            )
    return lines


def find_line(lines, keywords):
    for line in lines:
        text = line["text"]
        if all(keyword in text for keyword in keywords):
            return line
    return None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--contact", default="张子豪")
    parser.add_argument("--message", default="OwO 受控测试-真实环境-001")
    args = parser.parse_args()

    post("/perception/layers", {"layer": "l2_visual", "enabled": True})
    post("/desktop/activate", {"process": "qq", "title": "QQ"})
    time.sleep(0.6)

    # 1) 搜索联系人
    post("/desktop/shortcut", {"combo": "ctrl+f"})
    time.sleep(0.8)
    post("/desktop/type", {"text": args.contact})
    time.sleep(1.2)
    lines = ocr_lines(1105, 200, 1540, 945)
    contact_line = find_line(lines, [args.contact])
    if not contact_line:
        print(json.dumps(lines, ensure_ascii=False, indent=1), flush=True)
        raise RuntimeError("未在搜索结果中找到联系人：" + args.contact)
    cx = contact_line["x"] + contact_line["width"] // 2
    cy = contact_line["y"] + contact_line["height"] // 2
    print("[contact]", contact_line["text"], "@", cx, cy, flush=True)
    post("/desktop/click", {"x": cx, "y": cy})
    time.sleep(1.2)

    # 2) 找输入框与发送按钮（右侧聊天区）
    lines = ocr_lines(1530, 200, 1860, 945)
    input_line = find_line(lines, ["输入"])
    send_line = find_line(lines, ["发送"])
    if not input_line or not send_line:
        print(json.dumps(lines, ensure_ascii=False, indent=1), flush=True)
        raise RuntimeError("未找到输入框/发送按钮：input={} send={}".format(input_line, send_line))
    ix = input_line["x"] + input_line["width"] // 2
    iy = input_line["y"] + input_line["height"] // 2
    sx = send_line["x"] + send_line["width"] // 2
    sy = send_line["y"] + send_line["height"] // 2
    print("[input]", input_line["text"], "@", ix, iy, flush=True)
    print("[send]", send_line["text"], "@", sx, sy, flush=True)

    # 3) 输入并发送
    post("/desktop/click", {"x": ix, "y": iy})
    time.sleep(0.4)
    post("/desktop/type", {"text": args.message})
    time.sleep(0.4)
    post("/desktop/click", {"x": sx, "y": sy})
    time.sleep(1.5)

    # 4) 验证：消息出现在聊天区 + 输入框已清空
    lines = ocr_lines(1530, 200, 1860, 945)
    sent_visible = any(args.message in line["text"] for line in lines)
    input_still = find_line(lines, ["输入"])
    input_cleared = input_still is None
    report = {
        "ok": sent_visible,
        "contact": args.contact,
        "message": args.message,
        "message_visible": sent_visible,
        "input_cleared": input_cleared,
        "chat_lines": [line["text"] for line in lines[-12:]],
    }
    print(json.dumps(report, ensure_ascii=False, indent=2), flush=True)
    return 0 if sent_visible else 1


if __name__ == "__main__":
    sys.exit(main())
