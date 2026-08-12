#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""模拟 QQ 操作记忆闭环验收：示范 → 录制 → 泛化 → 沉淀技能包 → 换参数复用执行。

前置：owo-sim-qq --headless + 核心服务（OWO_SIM_QQ_URL 指向模拟窗口）。

流程：
  1. 脚本化示范一次“点击输入框 → 输入 → 回车发送 → 等待回复 → 再次发送”。
  2. 读取模拟日志，映射为 RecordedAction（内容掩码，只记动作摘要）。
  3. POST /learn/start + /learn/record* + /learn/stop + /learn/sink → qq_reply 技能包（Type 泛化为 {value}）。
  4. /reset 重置场景，用新 {value} 执行 /learn/execute-package（走模拟面执行器源）。
  5. 断言：执行报告 ok、日志出现新消息、输入框清空。
"""
import argparse
import json
import sys
import time
import urllib.request
from datetime import datetime, timezone

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")


def http_json(method, url, body=None, timeout=120):
    data = None if body is None else json.dumps(body, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(url, data=data, method=method)
    request.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.loads(response.read().decode("utf-8"))


def sim_lines(sim):
    ocr = http_json("GET", sim + "/ocr", timeout=10)
    return ocr.get("lines", [])


def line_center(line):
    return (
        int(line["x"]) + int(line["width"]) // 2,
        int(line["y"]) + int(line["height"]) // 2,
    )


def find_line(lines, text, role=None):
    for line in lines:
        if text in line.get("text", ""):
            if role and line.get("role_hint") != role:
                continue
            return line
    return None


def record_action(base, action_type, name, role, value_masked=True):
    anchor = {"app_id": "qq", "name": name}
    if role:
        anchor["role"] = role
    return http_json(
        "POST",
        base + "/learn/record",
        {
            "action": {
                "app_id": "qq",
                "anchor": anchor,
                "action_type": action_type,
                "value_masked": value_masked,
                "sensitive": False,
                "at": datetime.now(timezone.utc).isoformat(),
            }
        },
        timeout=30,
    )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default="http://127.0.0.1:4096")
    parser.add_argument("--sim", default="http://127.0.0.1:18500")
    parser.add_argument("--value", default="技能复用验证-001")
    parser.add_argument("--skill", default="qq_reply")
    args = parser.parse_args()

    http_json("POST", args.sim + "/reset", {}, timeout=10)
    time.sleep(1.2)

    # 1) 脚本化示范：两次“点击输入框 → 输入 → 回车发送”
    def demo_round(text):
        lines = sim_lines(args.sim)
        input_line = find_line(lines, "输入消息", "input")
        if not input_line:
            raise RuntimeError("模拟窗口未找到输入框：" + json.dumps(lines, ensure_ascii=False)[:300])
        x, y = line_center(input_line)
        http_json("POST", args.sim + "/click", {"x": x, "y": y}, timeout=10)
        http_json("POST", args.sim + "/type", {"text": text}, timeout=10)
        http_json("POST", args.sim + "/key", {"key": "enter"}, timeout=10)

    demo_round("示范消息一：今晚吃粥")
    time.sleep(1)
    demo_round("示范消息二：好的，六点半见")

    log = http_json("GET", args.sim + "/log", timeout=10)["entries"]
    events = [
        (entry.get("type"), entry)
        for entry in log
        if entry.get("type")
        in ("input_clicked", "typed", "send_clicked", "contact_switched")
    ]
    if not any(kind == "typed" for kind, _ in events) or not any(
        kind == "send_clicked" for kind, _ in events
    ):
        raise RuntimeError("示范日志缺少 typed/send_clicked：" + json.dumps(log, ensure_ascii=False))

    # 2) 录制（内容掩码：只记动作摘要，不记消息正文）
    http_json("POST", args.base + "/learn/start", {}, timeout=30)
    for kind, entry in events:
        if kind == "input_clicked" or kind == "typed":
            record_action(args.base, "click" if kind == "input_clicked" else "type", "输入消息", "edit")
        elif kind == "send_clicked":
            record_action(args.base, "click", "发送", "button")
        elif kind == "contact_switched":
            record_action(
                args.base, "click", entry.get("contact", ""), "list"
            )
    stop = http_json("POST", args.base + "/learn/stop", {}, timeout=30)
    print("[learn] samples:", stop.get("samples"), flush=True)

    # 3) 沉淀技能包
    sink = http_json(
        "POST",
        args.base + "/learn/sink",
        {
            "name": args.skill,
            "target_apps": ["qq"],
            "sensitivity": "medium",
            "description": "模拟 QQ 回复：点击输入框→输入→点击发送（可复用）",
        },
        timeout=30,
    )
    print("[learn] sink:", json.dumps(sink, ensure_ascii=False), flush=True)
    detail = http_json(
        "GET", args.base + "/learn/packages/" + args.skill, timeout=30
    )
    variables = detail.get("variables", [])
    print("[learn] package variables:", variables, flush=True)
    if "value" not in variables:
        raise RuntimeError("技能包未泛化出 {value} 变量：" + json.dumps(detail, ensure_ascii=False))

    # 4) 重置场景，换参数复用执行
    http_json("POST", args.sim + "/reset", {}, timeout=10)
    time.sleep(1.2)
    execute = http_json(
        "POST",
        args.base + "/learn/execute-package",
        {
            "name": args.skill,
            "variables": {"value": args.value},
            "confirm": True,
            "max_steps": 20,
        },
        timeout=120,
    )
    print(
        "[reuse] ok={} steps={}".format(
            execute.get("ok"),
            [step.get("status") for step in execute.get("steps", [])],
        ),
        flush=True,
    )
    if not execute.get("ok"):
        print(json.dumps(execute, ensure_ascii=False, indent=2), flush=True)
        return 1

    # 5) 断言
    time.sleep(0.5)
    log2 = http_json("GET", args.sim + "/log", timeout=10)["entries"]
    outgoing = [entry.get("text") for entry in log2 if entry.get("type") == "outgoing"]
    state = http_json("GET", args.sim + "/state", timeout=10)
    passed = args.value in outgoing and state.get("input") == ""
    report = {
        "ok": passed,
        "skill": args.skill,
        "variables": variables,
        "reuse_outgoing": outgoing,
        "value_sent": args.value in outgoing,
        "input_after": state.get("input"),
        "exec_steps": execute.get("steps"),
    }
    print(json.dumps(report, ensure_ascii=False, indent=2), flush=True)
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
