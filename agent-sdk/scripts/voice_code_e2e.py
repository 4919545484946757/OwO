"""VSCode 语音改代码 E2E 驱动（仅依赖标准库）。

流程：创建会话 -> 提交转写后的提示 -> 流式消费 SSE 事件 -> 审批放行 -> 读取目标文件验证。
"""
import json
import os
import socket
import sys
import time
import urllib.request


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


def main() -> int:
    endpoint = os.environ["E2E_ENDPOINT"]
    workspace = os.environ["E2E_WORKSPACE"]
    stt_json = os.environ.get("E2E_STT_JSON")
    prompt = os.environ.get("E2E_PROMPT", "")
    if stt_json:
        with open(stt_json, encoding="utf-8") as f:
            prompt = json.load(f)["text"]
    target_file = os.environ["E2E_TARGET"]
    timeout_s = int(os.environ.get("E2E_TIMEOUT", "120"))

    session = post_json(endpoint, "/session", {"workspace": workspace})
    session_id = session["id"]
    print("session:", session_id, flush=True)

    body = json.dumps({"prompt": prompt}).encode("utf-8")
    request = urllib.request.Request(
        endpoint + f"/session/{session_id}/turn",
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    deadline = time.time() + timeout_s
    final_text = None
    with urllib.request.urlopen(request, timeout=timeout_s) as response:
        current_event = None
        data_lines = []
        while time.time() < deadline:
            try:
                line = response.readline()
            except socket.timeout:
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
                    log_path = os.environ.get("E2E_LOG")
                    if log_path:
                        with open(log_path, "a", encoding="utf-8") as log:
                            log.write(f"{current_event}\t{data}\n")
                    if current_event == "permission_request":
                        post_json(
                            endpoint,
                            f"/session/{session_id}/permission/{payload['request_id']}",
                            {"allow": True},
                        )
                        print("approved:", payload.get("tool"), flush=True)
                    elif current_event == "final":
                        final_text = payload.get("text")
                        print("final received", flush=True)
                    elif current_event == "progress":
                        print("progress:", payload.get("message", "")[:120], flush=True)
                    current_event = None
                continue
            if text.startswith("event:"):
                current_event = text[6:].strip()
            elif text.startswith("data:"):
                data_lines.append(text[5:].strip())

    print("final_text:", (final_text or "")[:200].encode("unicode_escape").decode(), flush=True)
    if not os.path.exists(target_file):
        print("RESULT: FAIL target missing", flush=True)
        return 1
    with open(target_file, encoding="utf-8-sig") as f:
        content = f.read()
    ok = "def add" in content and "return" in content
    print("file:", target_file, flush=True)
    result_path = os.environ.get("E2E_RESULT_FILE")
    if result_path:
        with open(result_path, "w", encoding="utf-8") as f:
            f.write(content)
    print("RESULT: " + ("PASS" if ok else "FAIL"), flush=True)
    return 0 if ok else 2


if __name__ == "__main__":
    sys.exit(main())
