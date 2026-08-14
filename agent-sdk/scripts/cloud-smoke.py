#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M4a 云端执行 CLI 冒烟：mock 传输上跑 submit→run→status→diff→apply→revert 全链路。

用法：python scripts/cloud-smoke.py [--bin target/debug/owo-agent.exe] [--dir <临时队列目录>]
前置：owo-agent 已构建（cargo build -p owo-agent-cli）。
"""
import argparse
import json
import os
import subprocess
import sys
import tempfile

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bin", default=os.path.join("target", "debug", "owo-agent.exe"))
    parser.add_argument("--dir", default=None)
    args = parser.parse_args()

    work = tempfile.mkdtemp(prefix="owo-cloud-smoke-")
    if args.dir:
        work = args.dir
    ws = os.path.join(work, "ws")
    os.makedirs(ws, exist_ok=True)
    with open(os.path.join(ws, "a.txt"), "w", encoding="utf-8") as fh:
        fh.write("old\n")

    common = [args.bin, "cloud", "--dir", os.path.join(work, "queue"), "--transport", "mock"]

    def run(*extra):
        result = subprocess.run(
            common + list(extra), capture_output=True, text=True, encoding="utf-8", timeout=300
        )
        print("$", " ".join(common[-1:] + list(extra)), flush=True)
        print(result.stdout, flush=True)
        if result.returncode != 0:
            print(result.stderr, flush=True)
            raise SystemExit(result.returncode)
        return result.stdout

    run("submit", "--workspace", ws, "--command", "echo new > a.txt", "--command", "echo x > b.txt", "--run")
    out = run("list")
    assert "Succeeded" in out, "list 应显示任务终态"
    out = run("status", "cloud-0001")
    assert "Succeeded" in out, "status 应显示 Succeeded"
    out = run("diff", "cloud-0001")
    assert "a.txt" in out and "b.txt" in out, "diff 应列出变更文件"
    run("apply", "cloud-0001", "--workspace", ws)
    with open(os.path.join(ws, "a.txt"), encoding="utf-8") as fh:
        assert fh.read().strip() == "new", "apply 后 a.txt 应为 new"
    assert os.path.exists(os.path.join(ws, "b.txt")), "apply 后 b.txt 应存在"
    run("revert", "cloud-0001", "--workspace", ws)
    with open(os.path.join(ws, "a.txt"), encoding="utf-8") as fh:
        assert fh.read().strip() == "old", "revert 后 a.txt 应还原 old"
    assert not os.path.exists(os.path.join(ws, "b.txt")), "revert 后 b.txt 应删除"

    # 危险命令拒绝（退出码非 0 且报错）
    result = subprocess.run(
        common + ["submit", "--workspace", ws, "--command", "shutdown /s"],
        capture_output=True, text=True, encoding="utf-8", timeout=60,
    )
    assert result.returncode != 0, "危险命令应被拒绝"
    print("危险命令被拒绝：", result.stderr.strip(), flush=True)

    print("CLOUD SMOKE PASS", flush=True)


if __name__ == "__main__":
    main()
