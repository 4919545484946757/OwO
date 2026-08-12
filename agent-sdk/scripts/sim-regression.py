#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""模拟回归门禁：顺序跑确定性闭环套件（学习/观察），可选含 LLM 的套件。

用法：
  python scripts/sim-regression.py --base http://127.0.0.1:4097 --sim http://127.0.0.1:18500
  python scripts/sim-regression.py --with-llm   # 追加 QQ 单轮/浏览器模拟（需 DeepSeek 稳定）
"""
import argparse
import json
import os
import subprocess
import sys

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")


ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PYTHON = sys.executable


def run_suite(name, args, timeout=600):
    print(f"[suite] {name} ...", flush=True)
    try:
        result = subprocess.run(
            [PYTHON, os.path.join(ROOT, "scripts", args[0])] + args[1:],
            cwd=ROOT,
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=timeout,
        )
        ok = result.returncode == 0
        tail = (result.stdout + result.stderr).strip().splitlines()[-6:]
        print("\n".join(tail), flush=True)
        print(f"[suite] {name}: {'PASS' if ok else 'FAIL'} (rc={result.returncode})", flush=True)
        return ok
    except subprocess.TimeoutExpired:
        print(f"[suite] {name}: TIMEOUT", flush=True)
        return False


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default="http://127.0.0.1:4097")
    parser.add_argument("--sim", default="http://127.0.0.1:18500")
    parser.add_argument("--browser", default="http://127.0.0.1:18201")
    parser.add_argument("--with-llm", action="store_true")
    args = parser.parse_args()

    deterministic = [
        (
            "qq-learn",
            [
                "sim-qq-learn-e2e.py",
                "--base",
                args.base,
                "--sim",
                args.sim,
                "--value",
                "回归复用验证-001",
            ],
            300,
        ),
        (
            "qq-observe",
            [
                "sim-qq-observe-e2e.py",
                "--base",
                args.base,
                "--sim",
                args.sim,
                "--value",
                "回归观察验证-001",
            ],
            300,
        ),
    ]
    llm_suites = [
        (
            "qq-single",
            [
                "sim-qq-e2e.py",
                "--base",
                args.base,
                "--sim",
                args.sim,
                "--workspace",
                ROOT,
            ],
            900,
        ),
        (
            "browser-sim",
            [
                "sim-browser-e2e.py",
                "--base",
                args.base,
                "--browser",
                args.browser,
                "--workspace",
                ROOT,
            ],
            900,
        ),
    ]
    suites = deterministic + (llm_suites if args.with_llm else [])
    results = {}
    for name, argv, timeout in suites:
        results[name] = run_suite(name, argv, timeout)
    print(json.dumps(results, ensure_ascii=False, indent=2), flush=True)
    return 0 if all(results.values()) else 1


if __name__ == "__main__":
    sys.exit(main())
