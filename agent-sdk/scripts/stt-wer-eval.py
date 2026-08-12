# -*- coding: utf-8 -*-
"""STT WER/CER 评估工具：清单（wav<TAB>标准文本）→ 本地 /stt/transcribe → 聚合 CER/WER。

用法：
  python stt-wer-eval.py --endpoint http://127.0.0.1:4097 --manifest eval.tsv --out report.json
清单格式：每行 `<wav 路径>TAB<标准文本>`（UTF-8）。
"""
import argparse
import json
import os
import re
import sys
import urllib.request


def normalize(text: str) -> str:
    # 去掉标点与空白（中文按字符计算 CER；英文按词计算 WER 时另行分词）
    return re.sub(r"[\s，。！？,.!?、：:；;\"'“”（）()【】\[\]·\-_/\\]", "", text)


def levenshtein(a: str, b: str) -> int:
    dp = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        prev = dp[0]
        dp[0] = i
        for j, cb in enumerate(b, 1):
            cur = dp[j]
            dp[j] = min(dp[j] + 1, dp[j - 1] + 1, prev + (ca != cb))
            prev = cur
    return dp[-1]


def contains_cjk(text: str) -> bool:
    return any("\u4e00" <= ch <= "\u9fff" for ch in text)


def word_error_rate(reference: str, hypothesis: str) -> float:
    ref_words = reference.split()
    hyp_words = hypothesis.split()
    dp = list(range(len(hyp_words) + 1))
    for i, ref_word in enumerate(ref_words, 1):
        prev = dp[0]
        dp[0] = i
        for j, hyp_word in enumerate(hyp_words, 1):
            cur = dp[j]
            dp[j] = min(dp[j] + 1, dp[j - 1] + 1, prev + (ref_word != hyp_word))
            prev = cur
    return dp[-1] / max(len(ref_words), 1)


def transcribe(endpoint: str, wav_path: str) -> str:
    with open(wav_path, "rb") as f:
        data = f.read()
    request = urllib.request.Request(
        endpoint.rstrip("/") + "/stt/transcribe",
        data=data,
        headers={"Content-Type": "audio/wav"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=120) as response:
        payload = json.loads(response.read().decode("utf-8"))
    if not payload.get("ok"):
        raise RuntimeError(payload.get("error", "unknown error"))
    return payload.get("text", "")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--endpoint", default="http://127.0.0.1:4097")
    parser.add_argument("--manifest", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()

    samples = []
    with open(args.manifest, encoding="utf-8") as f:
        for lineno, line in enumerate(f, 1):
            line = line.rstrip("\n")
            if not line.strip() or line.startswith("#"):
                continue
            parts = line.split("\t")
            if len(parts) < 2:
                print(f"跳过第 {lineno} 行（缺少制表符）", file=sys.stderr)
                continue
            wav_path, reference = parts[0], "\t".join(parts[1:])
            if not os.path.exists(wav_path):
                print(f"跳过不存在文件：{wav_path}", file=sys.stderr)
                continue
            try:
                hypothesis = transcribe(args.endpoint, wav_path)
            except Exception as error:  # noqa: BLE001
                print(f"转写失败 {wav_path}：{error}", file=sys.stderr)
                continue
            ref_norm = normalize(reference)
            hyp_norm = normalize(hypothesis)
            dist = levenshtein(ref_norm, hyp_norm)
            cer = dist / max(len(ref_norm), 1)
            cjk = contains_cjk(ref_norm)
            if cjk:
                wer = cer
            else:
                wer = word_error_rate(reference, hypothesis)
            samples.append(
                {
                    "file": wav_path,
                    "reference": reference,
                    "hypothesis": hypothesis,
                    "cer": round(cer, 4),
                    "wer": round(wer, 4),
                    "cjk": cjk,
                }
            )
            print(f"{os.path.basename(wav_path)} CER={cer:.2%} WER={wer:.2%}")

    if not samples:
        print("没有可评估的样本", file=sys.stderr)
        return 1
    cer_mean = sum(s["cer"] for s in samples) / len(samples)
    wer_mean = sum(s["wer"] for s in samples) / len(samples)
    report = {
        "samples": samples,
        "aggregate": {
            "count": len(samples),
            "cer_mean": round(cer_mean, 4),
            "wer_mean": round(wer_mean, 4),
        },
    }
    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(report, f, ensure_ascii=False, indent=2)
    print(f"汇总：{len(samples)} 个样本，平均 CER={cer_mean:.2%}，平均 WER={wer_mean:.2%}")
    print(f"报告：{args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
