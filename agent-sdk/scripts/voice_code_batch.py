# -*- coding: utf-8 -*-
"""VSCode 语音改代码批量跑分：每轮重置 hello.py -> STT 转写提示 -> DeepSeek Agent 修改 -> 验证。

环境变量：
  E2E_ENDPOINT / E2E_WORKSPACE / E2E_TARGET / E2E_STT_JSON（或 E2E_PROMPT）
  E2E_ROUNDS（默认 10）/ E2E_TIMEOUT（默认 180）
输出：E2E_OUT（默认 <workspace>/../voice-code-batch.json）
"""
import json
import os
import socket
import sys
import time
import urllib.request
from concurrent.futures import ThreadPoolExecutor, TimeoutError as FutureTimeout


def post_json(endpoint, path, payload=None):
    data = json.dumps(payload).encode("utf-8") if payload is not None else None
    request = urllib.request.Request(
        endpoint + path,
        data=data,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.loads(response.read().decode("utf-8"))


STUB = 'def greet():\n    return "hello"\n'


def run_one(endpoint, workspace, target, prompt, timeout_s, log_path, func_name):
    with open(target, "w", encoding="utf-8") as f:
        f.write(STUB)
    session = post_json(endpoint, "/session", {"workspace": workspace})
    session_id = session["id"]
    body = json.dumps({"prompt": prompt}).encode("utf-8")
    request = urllib.request.Request(
        endpoint + f"/session/{session_id}/turn",
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    deadline = time.time() + timeout_s
    current_event = None
    data_lines = []
    final_text = None
    with urllib.request.urlopen(request, timeout=timeout_s) as response:
        while time.time() < deadline:
            try:
                line = response.readline()
            except (TimeoutError, socket.timeout):
                continue
            if not line:
                break
            text = line.decode("utf-8", errors="replace").rstrip("\r\n")
            if not text:
                if data_lines:
                    data = "".join(data_lines)
                    data_lines = []
                    try:
                        payload = json.loads(data)
                    except json.JSONDecodeError:
                        current_event = None
                        continue
                    if log_path:
                        with open(log_path, "a", encoding="utf-8") as log:
                            log.write(f"{current_event}\t{data}\n")
                    if current_event == "permission_request":
                        post_json(
                            endpoint,
                            f"/session/{session_id}/permission/{payload['request_id']}",
                            {"allow": True},
                        )
                    elif current_event == "final":
                        final_text = payload.get("text")
                    current_event = None
                continue
            if text.startswith("event:"):
                current_event = text[6:].strip()
            elif text.startswith("data:"):
                data_lines.append(text[5:].strip())
    with open(target, encoding="utf-8-sig") as f:
        content = f.read()
    ok = f"def {func_name}" in content and "return" in content
    return ok, final_text


def main() -> int:
    endpoint = os.environ["E2E_ENDPOINT"]
    workspace = os.environ["E2E_WORKSPACE"]
    target = os.environ["E2E_TARGET"]
    rounds = int(os.environ.get("E2E_ROUNDS", "10"))
    timeout_s = int(os.environ.get("E2E_TIMEOUT", "180"))
    func_name = os.environ.get("E2E_FUNC", "add")
    log_path = os.environ.get("E2E_LOG")
    out_path = os.environ.get("E2E_OUT", os.path.join(os.path.dirname(target), "..", "voice-code-batch.json"))
    stt_json = os.environ.get("E2E_STT_JSON")
    if stt_json:
        with open(stt_json, encoding="utf-8") as f:
            prompt = json.load(f)["text"]
    else:
        prompt = os.environ["E2E_PROMPT"]
    if log_path and os.path.exists(log_path):
        os.remove(log_path)

    results = []
    executor = ThreadPoolExecutor(max_workers=1)
    for i in range(1, rounds + 1):
        started = time.time()
        try:
            future = executor.submit(
                run_one, endpoint, workspace, target, prompt, timeout_s, log_path, func_name
            )
            try:
                ok, final_text = future.result(timeout=timeout_s + 15)
            except FutureTimeout:
                ok, final_text = False, None
                print(f"round {i}/{rounds}: TIMEOUT（硬看门狗）", flush=True)
            elapsed = round(time.time() - started, 1)
            results.append({"round": i, "ok": ok, "elapsed_s": elapsed, "final_text": final_text or ""})
            print(f"round {i}/{rounds}: {'PASS' if ok else 'FAIL'} ({elapsed}s)", flush=True)
        except Exception as error:  # noqa: BLE001
            elapsed = round(time.time() - started, 1)
            results.append({"round": i, "ok": False, "elapsed_s": elapsed, "error": str(error)})
            print(f"round {i}/{rounds}: ERROR {error}", flush=True)
    executor.shutdown(wait=False)

    passed = sum(1 for r in results if r["ok"])
    report = {
        "total": len(results),
        "passed": passed,
        "rate": round(passed / max(len(results), 1), 4),
        "results": results,
    }
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(report, f, ensure_ascii=False, indent=2)
    print(f"summary: {passed}/{len(results)} = {report['rate']:.0%} -> {out_path}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
