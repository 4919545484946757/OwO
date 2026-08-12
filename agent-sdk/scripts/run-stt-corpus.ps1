# STT 回归语料门禁：跑 tests\stt-corpus\corpus.tsv 并输出聚合报告。
param()
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$manifest = Join-Path $root "tests\stt-corpus\corpus.tsv"
$out = Join-Path $root "dist\stt-corpus-report.json"
& (Join-Path $PSScriptRoot "eval-stt-wer.ps1") -Manifest $manifest -OutJson $out
Write-Host "[stt-corpus] 报告：$out"
