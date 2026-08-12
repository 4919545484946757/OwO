#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""静默观察闭环验收：观察器自动入库 → /memory/mine-skill 挖掘技能包 → 换参数复用执行。

前置：owo-sim-qq --headless + 核心服务（OWO_SIM_QQ_URL 指向模拟窗口，
内存观察器随服务启动，每 2s 拉取模拟日志写入情景记忆）。

流程：
  1. /reset 清空场景 + /memory/clear 清空情景记忆。
  2. 脚本化示范两次“点击输入框 → 输入 → 回车发送”（观察器静默入库，不经过 /learn/record）。
  3. 轮询 /memory/observations 等到动作摘要入库。
  4. /memory/mine-skill 自动挖掘 → qq_reply_observed 技能包（{value}）。
  5. 重置场景，换参数执行技能包，断言发送成功。
"""
import argparse
import json
import sys
import time
import urllib.request

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


def demo_round(sim, text):
    lines = sim_lines(sim)
    input_line = next(
        (line for line in lines if "输入消息" in line.get("text", "")),
        None,
    )
    if not input_line:
        raise RuntimeError("模拟窗口未找到输入框")
    x, y = line_center(input_line)
    http_json("POST", sim + "/click", {"x": x, "y": y}, timeout=10)
    http_json("POST", sim + "/type", {"text": text}, timeout=10)
    http_json("POST", sim + "/key", {"key": "enter"}, timeout=10)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default="http://127.0.0.1:4096")
    parser.add_argument("--sim", default="http://127.0.0.1:18500")
    parser.add_argument("--value", default="观察复用验证-001")
    parser.add_argument("--skill", default="qq_reply_observed")
    args = parser.parse_args()

    http_json("POST", args.sim + "/reset", {}, timeout=10)
    http_json("POST", args.base + "/memory/clear", {}, timeout=10)
    time.sleep(1.2)

    demo_round(args.sim, "观察示范一：今晚吃粥")
    time.sleep(1)
    demo_round(args.sim, "观察示范二：好的，六点半见")

    # 等待观察器入库（2s 轮询间隔，最多 15s）
    seen_kinds = set()
    deadline = time.time() + 15
    while time.time() < deadline:
        memory = http_json("GET", args.base + "/memory/observations?limit=200", timeout=10)
        seen_kinds = {
            obs.get("detail", {}).get("type")
            for obs in memory.get("observations", [])
        }
        if {"typed", "send_clicked"}.issubset(seen_kinds):
            break
        time.sleep(2)
    print("[observe] memory total:", memory.get("total"), "kinds:", sorted(seen_kinds), flush=True)
    if not {"typed", "send_clicked"}.issubset(seen_kinds):
        raise RuntimeError("观察器未在时限内入库 typed/send_clicked")

    mine = http_json(
        "POST",
        args.base + "/memory/mine-skill",
        {
            "name": args.skill,
            "target_apps": ["qq"],
            "sensitivity": "medium",
            "description": "静默观察挖掘：模拟 QQ 回复（点击输入框→输入→发送）",
        },
        timeout=30,
    )
    print("[mine] ", json.dumps(mine, ensure_ascii=False), flush=True)
    variables = mine.get("variables", [])
    if "value" not in variables:
        raise RuntimeError("挖掘出的技能包缺少 {value} 变量")

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
    if not execute.get("ok"):
        print(json.dumps(execute, ensure_ascii=False, indent=2), flush=True)
        return 1
    time.sleep(0.5)
    log = http_json("GET", args.sim + "/log", timeout=10)["entries"]
    outgoing = [entry.get("text") for entry in log if entry.get("type") == "outgoing"]
    state = http_json("GET", args.sim + "/state", timeout=10)
    passed = args.value in outgoing and state.get("input") == ""
    report = {
        "ok": passed,
        "skill": args.skill,
        "variables": variables,
        "memory_observations": memory.get("total"),
        "observed_kinds": sorted(seen_kinds),
        "reuse_outgoing": outgoing,
        "value_sent": args.value in outgoing,
        "input_after": state.get("input"),
    }
    print(json.dumps(report, ensure_ascii=False, indent=2), flush=True)
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
