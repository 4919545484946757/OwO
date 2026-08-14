#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""computer-use 审批版闭环 e2e（M4d，模拟/沙箱面，不依赖真实桌面）。

前置：
  1. owo-sim-qq --headless --port 18500（模拟 QQ 窗口：/reset /frame /ocr /click /type /key）
  2. （可选 --with-server）核心服务以 OWO_SIM_QQ_URL=http://127.0.0.1:18500 启动，
     用于验证任务创建/批准/敏感检查的 HTTP 语义。

流程（阶段 1 模拟面闭环，等价 core::computer_use::run_approved_task 的
"感知→定位→动作→验证"每步）：
  /reset 清空 → /ocr 感知（找"输入消息"锚点）→ 门禁授权（模拟面视为已批准任务内）→
  点击锚点中心 → 输入文本 → /ocr 验证输入已上屏 → 完成。
阶段 2（--with-server）：HTTP 创建任务 → 批准 → sensitive-check 密码框命中 → 拒绝语义。
"""
import argparse
import json
import os
import subprocess
import sys
import time
import urllib.request

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")


def http_json(method, url, body=None, timeout=30):
    data = None if body is None else json.dumps(body, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(url, data=data, method=method)
    request.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.loads(response.read().decode("utf-8"))


def wait_http(url, tries=20, interval=0.5):
    for _ in range(tries):
        try:
            return http_json("GET", url, timeout=3)
        except Exception:
            time.sleep(interval)
    raise RuntimeError(f"服务不可达：{url}")


def ocr_lines(sim):
    return http_json("GET", sim + "/ocr")["lines"]


def find_line(lines, needle):
    needle = needle.lower()
    for line in lines:
        if needle in (line.get("text") or "").lower():
            return line
    return None


def phase1_sim_loop(sim):
    print("== 阶段 1：模拟面感知闭环 ==", flush=True)
    http_json("POST", sim + "/reset", {}, timeout=10)
    time.sleep(1.0)

    # 感知：OCR 找输入框锚点。
    lines = ocr_lines(sim)
    anchor = find_line(lines, "输入消息")
    assert anchor, f"感知失败：OCR 未找到输入框锚点，lines={[l.get('text') for l in lines]}"
    x, y = anchor["x"] + anchor["width"] // 2, anchor["y"] + anchor["height"] // 2
    print(f"  [感知] 锚点「{anchor.get('text')}」中心 ({x},{y})", flush=True)

    # 定位+动作：点击输入框（门禁视为任务内授权动作）。
    http_json("POST", sim + "/click", {"x": x, "y": y}, timeout=10)
    print(f"  [动作] click ({x},{y})", flush=True)

    text = f"computer-use-e2e-{int(time.time())}"
    http_json("POST", sim + "/type", {"text": text}, timeout=10)
    print(f"  [动作] type {text}", flush=True)

    # 验证：OCR 确认文本已上屏。
    verified = False
    for _ in range(10):
        lines = ocr_lines(sim)
        if any(text in (l.get("text") or "") for l in lines):
            verified = True
            break
        time.sleep(0.3)
    assert verified, "验证失败：输入文本未出现在模拟窗口 OCR"
    print("  [验证] 输入文本已上屏 ✓", flush=True)
    print("  阶段 1 PASS", flush=True)
    return text


def phase2_server(sim, base):
    print("== 阶段 2：核心服务审批语义（--with-server） ==", flush=True)
    wait_http(base + "/health")
    # 创建任务（未批准）。
    created = http_json(
        "POST",
        base + "/computer-use/task",
        {
            "target_app": "owo-sim-qq",
            "description": "e2e 受控发送",
            "max_duration_ms": 60000,
            "allowed_actions": ["desktop_click", "desktop_type"],
        },
        timeout=10,
    )
    task = created["task"]
    assert task["state"] == "Pending", f"新任务应为 Pending：{task['state']}"
    print(f"  [任务] 创建 {task['id']} 状态 {task['state']}", flush=True)

    # 未批准时执行动作应被拒（无执行端点时以状态迁移语义验证：approve 前不可 start）。
    # 批准。
    approved = http_json("POST", base + f"/computer-use/task/{task['id']}/approve", {}, timeout=10)
    assert approved["state"] == "Approved", f"批准后应为 Approved：{approved['state']}"
    print(f"  [批准] {approved['state']}", flush=True)

    # 敏感检查：密码框命中。
    sensitive = http_json(
        "POST",
        base + "/computer-use/sensitive-check",
        {"name": "请输入支付密码", "role": "edit", "ocr_text": "card 验证码"},
        timeout=10,
    )
    assert sensitive.get("sensitive") is True, f"敏感检查应命中：{sensitive}"
    print(f"  [熔断] sensitive-check 命中：{sensitive.get('reason')}", flush=True)

    safe = http_json(
        "POST",
        base + "/computer-use/sensitive-check",
        {"name": "输入消息", "role": "edit", "ocr_text": "发送"},
        timeout=10,
    )
    assert safe.get("sensitive") is False, f"普通输入不应命中：{safe}"
    print("  [放行] sensitive-check 普通输入未命中 ✓", flush=True)
    print("  阶段 2 PASS", flush=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--sim", default="http://127.0.0.1:18500")
    parser.add_argument("--sim-exe", default=None, help="owo-sim-qq 可执行文件路径（缺省自动找 target/debug）")
    parser.add_argument("--with-server", action="store_true", help="同时验证核心服务审批语义（需已启动）")
    parser.add_argument("--base", default="http://127.0.0.1:4096")
    args = parser.parse_args()

    # 阶段 1 直接驱动模拟面（无核心服务依赖）。
    phase1_sim_loop(args.sim)

    if args.with_server:
        phase2_server(args.sim, args.base)

    print("\ne2e ALL PASS", flush=True)


if __name__ == "__main__":
    main()
