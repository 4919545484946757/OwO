#!/usr/bin/env python3
"""官方示例插件：剪贴板读写（MCP stdio，JSON-RPC 2.0）。

Windows 下经 PowerShell Get-Clipboard/Set-Clipboard 实现；
其他平台返回明确提示，不静默失败。读接口只返回文本（忽略图片等格式）。
"""

import json
import subprocess
import sys
import io


def clipboard_read():
    try:
        result = subprocess.run(
            ["powershell.exe", "-NoProfile", "-Command", "Get-Clipboard -Raw"],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=5,
        )
        if result.returncode == 0:
            text = (result.stdout or "").rstrip("\r\n")
            return {"text": text}
        return {"text": "", "error": f"Get-Clipboard 失败（{result.returncode}）"}
    except FileNotFoundError:
        return {"text": "", "error": "当前平台不支持剪贴板读取（需要 Windows PowerShell）"}
    except subprocess.TimeoutExpired:
        return {"text": "", "error": "剪贴板读取超时"}


def clipboard_write(text):
    try:
        import base64

        # 避免命令行转义问题：经 base64 传给 PowerShell 解码写入。
        encoded = base64.b64encode(text.encode("utf-8")).decode("ascii")
        script = (
            "[Console]::InputEncoding=[Text.Encoding]::UTF8;"
            "$b=[Convert]::FromBase64String('%s');"
            "Set-Clipboard -Value ([Text.Encoding]::UTF8.GetString($b))" % encoded
        )
        result = subprocess.run(
            ["powershell.exe", "-NoProfile", "-Command", script],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=5,
        )
        if result.returncode == 0:
            return {"ok": True, "chars": len(text)}
        return {"ok": False, "error": f"Set-Clipboard 失败（{result.returncode}）"}
    except FileNotFoundError:
        return {"ok": False, "error": "当前平台不支持剪贴板写入（需要 Windows PowerShell）"}
    except subprocess.TimeoutExpired:
        return {"ok": False, "error": "剪贴板写入超时"}


def handle(method, message):
    identifier = message.get("id")
    if method == "initialize":
        return {"jsonrpc": "2.0", "id": identifier, "result": {
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "owo-clipboard", "version": "1.0.0"},
        }}
    if method == "tools/list":
        return {"jsonrpc": "2.0", "id": identifier, "result": {"tools": [
            {
                "name": "clipboard_read",
                "description": "读取当前剪贴板文本（仅文本，掩码由 Agent 层负责）",
                "inputSchema": {"type": "object", "properties": {}},
            },
            {
                "name": "clipboard_write",
                "description": "把文本写入剪贴板（权限 clipboard:write）",
                "inputSchema": {
                    "type": "object",
                    "properties": {"text": {"type": "string"}},
                    "required": ["text"],
                },
            },
        ]}}
    if method == "tools/call":
        params = message.get("params") or {}
        name = params.get("name")
        arguments = params.get("arguments") or {}
        if name == "clipboard_read":
            result = clipboard_read()
            return {"jsonrpc": "2.0", "id": identifier, "result": {
                "content": [{"type": "text", "text": json.dumps(result, ensure_ascii=False)}]
            }}
        if name == "clipboard_write":
            text = arguments.get("text") or ""
            result = clipboard_write(text)
            return {"jsonrpc": "2.0", "id": identifier, "result": {
                "content": [{"type": "text", "text": json.dumps(result, ensure_ascii=False)}]
            }}
        return {"jsonrpc": "2.0", "id": identifier, "error": {
            "code": -32602, "message": f"unknown tool: {name}"
        }}
    if method in ("notifications/initialized", "exit"):
        return None
    return {"jsonrpc": "2.0", "id": identifier, "error": {
        "code": -32601, "message": f"method not found: {method}"
    }}


def main():
    stdin = io.TextIOWrapper(sys.stdin.buffer, encoding="utf-8", errors="replace")
    stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8", errors="replace")
    for line in stdin:
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        method = message.get("method", "")
        response = handle(method, message)
        if response is None:
            if method == "exit":
                break
            continue
        stdout.write(json.dumps(response, ensure_ascii=False) + "\n")
        stdout.flush()


if __name__ == "__main__":
    main()
