#!/usr/bin/env python3
"""官方示例插件：翻译（MCP stdio，JSON-RPC 2.0）。

演示用途：内置常用短语词典，无网络依赖；未命中时返回带前缀的原文，
避免插件隐藏真实未翻译状态。协议与 owo-agent MCP 客户端兼容。
"""

import json
import sys
import io


DEMO_DICT = {
    "hello": "你好",
    "hi": "你好",
    "thanks": "谢谢",
    "thank you": "谢谢",
    "goodbye": "再见",
    "submit": "提交",
    "send": "发送",
    "search": "搜索",
    "save": "保存",
    "cancel": "取消",
    "file": "文件",
    "open": "打开",
    "close": "关闭",
    "confirm": "确认",
    "输入": "input",
    "发送": "send",
    "搜索": "search",
    "提交": "submit",
    "保存": "save",
    "取消": "cancel",
    "你好": "hello",
    "谢谢": "thanks",
    "再见": "goodbye",
    "文件": "file",
}


def translate(text, target):
    key = text.strip().lower()
    if target == "en" and key in DEMO_DICT:
        translated = DEMO_DICT[key]
    elif key in DEMO_DICT:
        translated = DEMO_DICT[key]
    else:
        translated = f"[演示翻译:{target}] {text}"
    return {"translated": translated, "engine": "demo-dict", "target": target}


def handle(method, message):
    identifier = message.get("id")
    if method == "initialize":
        return {"jsonrpc": "2.0", "id": identifier, "result": {
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "owo-translate", "version": "1.0.0"},
        }}
    if method == "tools/list":
        return {"jsonrpc": "2.0", "id": identifier, "result": {"tools": [
            {
                "name": "translate",
                "description": "把文本翻译为指定语言（演示词典：中英常用短语；未命中返回带前缀原文）",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "text": {"type": "string", "description": "要翻译的文本"},
                        "target": {"type": "string", "description": "目标语言（zh/en），默认 zh"},
                    },
                    "required": ["text"],
                },
            }
        ]}}
    if method == "tools/call":
        params = message.get("params") or {}
        name = params.get("name")
        arguments = params.get("arguments") or {}
        if name == "translate":
            text = arguments.get("text") or ""
            target = arguments.get("target") or "zh"
            return {"jsonrpc": "2.0", "id": identifier, "result": {
                "content": [{"type": "text", "text": json.dumps(translate(text, target), ensure_ascii=False)}]
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
