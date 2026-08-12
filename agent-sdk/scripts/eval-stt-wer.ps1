# STT WER/CER 评估：启动核心服务 → 对清单（wav<TAB>标准文本）逐条转写 → 聚合报告。
# 用法：powershell -ExecutionPolicy Bypass -File scripts\eval-stt-wer.ps1 `
#         -Manifest dist\stt-eval.tsv -OutJson dist\stt-eval-report.json
param(
    [Parameter(Mandatory = $true)]
    [string]$Manifest,
    [string]$OutJson = "",
    [int]$Port = 4098
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$root = Split-Path $PSScriptRoot -Parent
$runtimeRoot = if ($env:OWO_SKILL_RUNTIME) {
    $env:OWO_SKILL_RUNTIME
} else {
    "C:\Users\23843\.cache\codex-runtimes\codex-primary-runtime\dependencies"
}
$python = if ($env:OWO_SKILL_PYTHON) { $env:OWO_SKILL_PYTHON } else { Join-Path $runtimeRoot "python\python.exe" }
$exe = Join-Path $root "target\debug\owo-agent.exe"
if (-not (Test-Path $exe)) {
    throw "缺少核心服务：$exe（先运行 cargo build -p owo-agent-cli）"
}
if (-not (Test-Path $Manifest)) {
    throw "清单不存在：$Manifest"
}
if (-not $OutJson) {
    $OutJson = Join-Path (Split-Path $Manifest -Parent) "stt-eval-report.json"
}

$modelDir = Join-Path $env:LOCALAPPDATA "OwO\Agent\models\stt\SenseVoice-Small"
if (-not ((Test-Path (Join-Path $modelDir "model.int8.onnx")) -and (Test-Path (Join-Path $modelDir "tokens.txt")))) {
    Write-Host "[wer] 警告：SenseVoice 模型未就绪（运行 scripts\download-stt-model.ps1），评估会失败" -ForegroundColor Yellow
}

$env:OPENAI_API_KEY = "eval"
$env:OPENAI_BASE_URL = "http://127.0.0.1:9"
$env:OPENAI_MODEL = "mock"
$outLog = Join-Path $env:TEMP "owo-serve-wer-out.log"
$errLog = Join-Path $env:TEMP "owo-serve-wer-err.log"
$proc = Start-Process -FilePath $exe -ArgumentList @("serve", "--port", "$Port", "--workspace", $root) -WindowStyle Hidden -PassThru -RedirectStandardOutput $outLog -RedirectStandardError $errLog
try {
    $ready = $false
    for ($i = 0; $i -lt 40; $i++) {
        try {
            $null = Invoke-RestMethod -Uri "http://127.0.0.1:$Port/health" -TimeoutSec 1
            $ready = $true
            break
        } catch {
            Start-Sleep -Milliseconds 500
        }
    }
    if (-not $ready) {
        throw "核心服务未就绪：$(Get-Content $errLog -Raw -ErrorAction SilentlyContinue)"
    }
    & $python (Join-Path $PSScriptRoot "stt-wer-eval.py") --endpoint "http://127.0.0.1:$Port" --manifest $Manifest --out $OutJson
    if ($LASTEXITCODE -ne 0) {
        throw "评估失败（退出码 $LASTEXITCODE）"
    }
} finally {
    if ($proc -and -not $proc.HasExited) {
        Stop-Process -Id $proc.Id -Force
    }
}
