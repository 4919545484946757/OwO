# STT 回归语料（v0.4）

- `tts-zh.wav`：系统 TTS 普通话（文本即标准答案）
- `asr_example_zh.wav`：FunASR 官方真实人声（标准文本见 corpus.tsv）
- `mix1-3.wav`：中英混说 TTS 样本

标准文本见 `corpus.tsv`。运行：

```powershell
powershell -ExecutionPolicy Bypass -File scripts\run-stt-corpus.ps1
```

产出 `dist\stt-corpus-report.json`（逐样本 CER/WER + 聚合）。
