#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""模拟 QQ 桌面 Agent 端到端验收（后台静默版）。

前置：
  1. owo-sim-qq --headless --port 18500 --log <round>.jsonl
  2. 核心服务以 OWO_SIM_QQ_URL=http://127.0.0.1:18500 启动（建议 OWO_AUTO_APPROVE=1）
  3. DeepSeek/OpenAI 兼容密钥环境变量

流程：/reset 清空模拟窗口 → 建会话 → SSE 流式执行 turn →
      断言至少一次 send_clicked + 一次 outgoing + 输入框已清空。
"""
import argparse
import json
import os
import sys
import time
import urllib.request

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")


def http_json(method, url, body=None, timeout=60):
    data = None if body is None else json.dumps(body, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(url, data=data, method=method)
    request.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.loads(response.read().decode("utf-8"))


def preflight(base):
    try:
        health = http_json("GET", base + "/health", None, timeout=10)
    except Exception as exc:
        print(f"[preflight] 无法连接服务 {base}: {exc}", flush=True)
        sys.exit(2)
    if not health.get("auto_approve"):
        print(
            f"[preflight] {base} 未开启 OWO_AUTO_APPROVE=1：多轮工具调用会在审批处挂起 300s。"
            "模拟面请以 OWO_AUTO_APPROVE=1 重启服务。",
            flush=True,
        )
        sys.exit(2)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default="http://127.0.0.1:4096")
    parser.add_argument("--sim", default="http://127.0.0.1:18500")
    parser.add_argument("--prompt", default=None)
    parser.add_argument("--prompt-file", default=None)
    parser.add_argument("--workspace", default=os.getcwd())
    parser.add_argument(
        "--require-contacts",
        default="",
        help="逗号分隔的联系人名单：断言 outgoing 必须覆盖这些 to",
    )
    parser.add_argument(
        "--require-contacts-file",
        default=None,
        help="UTF-8 文本文件，内容为逗号分隔的联系人名单",
    )
    args = parser.parse_args()

    preflight(args.base)

    http_json("POST", args.sim + "/reset", {}, timeout=10)
    time.sleep(1.5)  # 等第一条 incoming 注入

    session = http_json(
        "POST",
        args.base + "/session",
        {
            "workspace": args.workspace,
            "model": None,
            "system_prompt": (
                "你是 OwO 桌面操作 Agent，当前操作虚拟模拟桌面（1020x700 模拟QQ窗口）。\n"
                "规则：\n"
                "0. 执行过程中不要输出任何分析文字或解释，直接调用工具完成任务；"
                "只有在全部步骤完成后才用中文简短报告结果。\n"
                "1. 界面理解只用 screen_ocr（返回 lines 带坐标和 role_hint），"
                "不要用 ocr_region 反复试探。\n"
                "2. 不要用 list_dir/search_files/read_file 等文件工具，除非任务明确要求改文件。\n"
                "3. 点击坐标取 lines 中目标行中心：x+width/2, y+height/2。\n"
                "4. 发送后必须用 screen_ocr 验证：输入框清空且消息上屏才算成功。\n"
                "5. 等待对方回复用 desktop_wait。\n"
                "6. 完成所有步骤后，用中文文字报告结果，不要只输出工具调用。"
            ),
        },
        timeout=30,
    )
    session_id = session["id"]
    if args.prompt_file:
        with open(args.prompt_file, "r", encoding="utf-8") as handle:
            prompt = handle.read()
    else:
        prompt = args.prompt or (
            "你现在操作一个模拟 QQ 聊天窗口（虚拟桌面，窗口大小为 1020x700）。请完成：\n"
            "1. 用 screen_ocr 读取屏幕。返回的 lines 是整行文本，带坐标和 role_hint；"
            "发送按钮行 role_hint=button，输入框行 role_hint=input。理解张子豪发来了什么消息。\n"
            "2. 找到输入框（占位文字'输入消息...'），点击它，输入一句合适的回复"
            "（对方在问今晚吃什么，且想吃清淡的）。\n"
            "3. 点击 role_hint=button 且文本为'发送'的行中心，再用 screen_ocr 验证：回复已上屏、输入框已清空。\n"
            "4. 用 desktop_wait 等几秒，再 screen_ocr 读取对方的新回复。\n"
            "5. 根据对方的新回复再回复一句并点击发送，再次验证。\n"
            "最后报告：你看到的聊天内容、发送的每条消息、验证结果。"
        )
    body = json.dumps({"prompt": prompt}, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(
        args.base + "/session/" + session_id + "/turn", data=body, method="POST"
    )
    request.add_header("Content-Type", "application/json")

    tool_uses = []
    final_text = None
    started = time.time()
    delta_total = 0
    with urllib.request.urlopen(request, timeout=1200) as response:
        for raw in response:
            line = raw.decode("utf-8", errors="replace").strip()
            if not line.startswith("data:"):
                continue
            payload = line[5:].strip()
            try:
                event = json.loads(payload)
            except json.JSONDecodeError:
                continue
            event_type = event.get("type")
            if event_type == "token_delta":
                delta_total += len(event.get("delta", ""))
                if delta_total % 200 < 50:
                    print("[delta] total={} tail={!r}".format(
                        delta_total,
                        event.get("delta", "")[-40:],
                    ), flush=True)
                continue
            if event_type == "tool_use":
                tool_uses.append(event.get("tool"))
                print(
                    "[tool] {} {}".format(
                        event.get("tool"),
                        json.dumps(event.get("args"), ensure_ascii=False)[:160],
                    ),
                    flush=True,
                )
            elif event_type == "tool_result":
                print(
                    "[result] {} ok={} err={}".format(
                        event.get("tool"), event.get("ok"), event.get("error")
                    ),
                    flush=True,
                )
            elif event_type == "final":
                final_text = event.get("text")
                print("[final] {}".format(final_text), flush=True)
    elapsed = time.time() - started

    state = http_json("GET", args.sim + "/state", None, timeout=10)
    log = http_json("GET", args.sim + "/log", None, timeout=10)["entries"]
    outgoing = [entry for entry in log if entry.get("type") == "outgoing"]
    send_clicks = [entry for entry in log if entry.get("type") == "send_clicked"]
    incoming = [entry for entry in log if entry.get("type") == "incoming"]
    passed = (
        len(outgoing) >= 1
        and len(send_clicks) >= 1
        and state.get("input") == ""
    )
    contacts_spec = args.require_contacts
    if args.require_contacts_file:
        with open(args.require_contacts_file, "r", encoding="utf-8") as handle:
            contacts_spec = handle.read()
    required_contacts = [
        item.strip() for item in contacts_spec.split(",") if item.strip()
    ]
    sent_to = {entry.get("to") for entry in outgoing}
    contacts_covered = all(contact in sent_to for contact in required_contacts)
    if required_contacts:
        passed = passed and contacts_covered
    report = {
        "ok": passed,
        "elapsed_s": round(elapsed, 1),
        "tools": sorted(set(tool_uses)),
        "delta_chars": delta_total,
        "outgoing_count": len(outgoing),
        "send_clicks": len(send_clicks),
        "incoming_count": len(incoming),
        "outgoing": [entry.get("text") for entry in outgoing],
        "required_contacts": required_contacts,
        "contacts_covered": contacts_covered,
        "input_after": state.get("input"),
        "final_text": final_text,
    }
    print(json.dumps(report, ensure_ascii=False, indent=2), flush=True)
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
