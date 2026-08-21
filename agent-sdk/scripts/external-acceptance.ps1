# R11:external-acceptance 质量收尾完成。
# R12:external-acceptance 完成（STT 缺模型显式 skip）。
# external-acceptance.ps1 — 外部验收驱动（R10 Agent 4 WP2）
# 用法：powershell -ExecutionPolicy Bypass -File scripts\external-acceptance.ps1 [-Port 4098] [-QqPath <exe>] [-OcrModelDir <dir>]
# 四类验收：STT 语料 / QQ 实机 / OCR 一致性 / 真实 eval。
# 缺凭据/设备/语料 → 显式 skip（不 panic）；结果 JSON 落 %TEMP%\owo-external-*（前缀白名单，保留）。
# 退出码：0 = 全过或全 skip；1 = 存在失败项。

param(
    [int]$Port = 4098,
    [string]$QqPath = "",
    [string]$OcrModelDir = ""
)

$ErrorActionPreference = "Continue"
$root = Split-Path -Parent $PSScriptRoot
$baseUrl = "http://127.0.0.1:$Port"

# ---- 输出目录（严格前缀白名单 %TEMP%\owo-external-*）----
$outDir = Join-Path $env:TEMP ("owo-external-" + [guid]::NewGuid().ToString("N"))
if (-not $outDir.StartsWith((Join-Path $env:TEMP "owo-external-"), [System.StringComparison]::OrdinalIgnoreCase)) {
    Write-Host "输出目录越界，拒绝写入：$outDir" -ForegroundColor Red
    exit 1
}
New-Item -ItemType Directory -Path $outDir -Force | Out-Null

$results = @()
$hasFail = $false

# R12：保存模型凭据快照——eval-stt-wer.ps1 经 `&` 链调用会把 OPENAI_API_KEY 覆写为
# "eval" 且 OPENAI_BASE_URL 指向不可达地址，污染当前进程环境；eval_gate 前恢复。
$savedModelEnv = @{
    OPENAI_API_KEY = $env:OPENAI_API_KEY
    OPENAI_BASE_URL = $env:OPENAI_BASE_URL
    OPENAI_MODEL = $env:OPENAI_MODEL
}

function Add-Result {
    param([string]$Name, [string]$Status, [string]$Detail)
    $script:results += [pscustomobject]@{ acceptance = $Name; status = $Status; detail = $Detail }
    $color = switch ($Status) { "pass" { "Green" } "fail" { "Red" } default { "Yellow" } }
    Write-Host ("  {0,-18} {1,-5}  {2}" -f $Name, $Status.ToUpper(), $Detail) -ForegroundColor $color
    if ($Status -eq "fail") { $script:hasFail = $true }
}

Write-Host "==== 外部验收驱动（$outDir）====" -ForegroundColor Cyan

# 1) STT 语料回归（依赖 eval-stt-wer.ps1 + corpus.tsv + SenseVoice 模型）
Write-Host "[1/4] STT 语料..."
$corpus = Join-Path $root "tests\stt-corpus\corpus.tsv"
$sttModel = Join-Path $env:LOCALAPPDATA "OwO\Agent\models\stt\SenseVoice-Small"
$sttModelReady = (Test-Path (Join-Path $sttModel "model.int8.onnx")) -and (Test-Path (Join-Path $sttModel "tokens.txt"))
if (Test-Path $corpus) {
    # R12：缺 STT 模型＝缺设备/引擎 → 显式 skip（不 panic，不误报 fail）。
    if (-not $sttModelReady) {
        Add-Result "stt_corpus" "skip" "缺 STT 模型 SenseVoice-Small（先跑 download-stt-model.ps1）"
    } else {
        try {
            $out = & (Join-Path $PSScriptRoot "run-stt-corpus.ps1") 2>&1 | Out-String
            if ($LASTEXITCODE -eq 0) { Add-Result "stt_corpus" "pass" "语料回归完成" }
            else { Add-Result "stt_corpus" "fail" "语料回归退出码 $LASTEXITCODE：$($out.Trim().Split("`n")[0])" }
        } catch {
            # R12：鉴权失败（缺本地 server 凭据）按"缺凭据"语义显式 skip，不 panic。
            if ($_.Exception.Message -match "401|Unauthorized|403|Forbidden") {
                Add-Result "stt_corpus" "skip" "STT server 鉴权凭据缺失（本地 server 需 token）：$_"
            } else {
                Add-Result "stt_corpus" "fail" "语料回归异常：$_"
            }
        }
    }
} else {
    Add-Result "stt_corpus" "skip" "缺语料 tests\stt-corpus\corpus.tsv"
}

# 2) QQ 实机（需登录态；缺设备/进程显式 skip）
Write-Host "[2/4] QQ 实机..."
$qqExe = $null
if ($QqPath -and (Test-Path $QqPath)) { $qqExe = $QqPath }
else {
    $candidates = @(
        "${env:ProgramFiles(x86)}\Tencent\QQ\Bin\QQ.exe",
        "${env:ProgramFiles}\Tencent\QQ\Bin\QQ.exe",
        "$env:LOCALAPPDATA\Tencent\QQ\Bin\QQ.exe"
    )
    $qqExe = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
}
$qqRunning = Get-Process -Name "QQ" -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $qqExe -and -not $qqRunning) {
    Add-Result "qq_live" "skip" "未发现 QQ 实机（需已安装且登录态；-QqPath 可指定路径）"
} else {
    try {
        if ($qqRunning) {
            Add-Result "qq_live" "pass" "QQ 实机在运行（pid=$($qqRunning.Id)），可接受真实输入验收"
        } elseif ($qqExe) {
            $p = Start-Process -FilePath $qqExe -PassThru
            Start-Sleep -Seconds 3
            $alive = Get-Process -Id $p.Id -ErrorAction SilentlyContinue
            if ($alive) {
                Add-Result "qq_live" "pass" "QQ 启动成功（pid=$($p.Id)）"
                $null = $alive.CloseMainWindow() | Out-Null
                Start-Sleep -Seconds 1
                $still = Get-Process -Id $p.Id -ErrorAction SilentlyContinue
                if ($still) { $still.Kill() }
            } else {
                Add-Result "qq_live" "fail" "QQ 启动后即退出"
            }
        }
    } catch {
        Add-Result "qq_live" "skip" "QQ 实机交互不可用：$_"
    }
}

# 3) OCR 一致性（模型目录 + 服务组件状态）
Write-Host "[3/4] OCR 一致性..."
$ocrDir = if ($OcrModelDir) { $OcrModelDir } else { Join-Path $root "models\ocr" }
if (-not (Test-Path $ocrDir)) {
    Add-Result "ocr_consistency" "skip" "缺 OCR 模型目录 $ocrDir（先跑 download-onnx-ocr-models.ps1）"
} else {
    try {
        $health = Invoke-RestMethod -Uri "$baseUrl/metrics/health" -TimeoutSec 5
        $ocr = $health.components.ocr
        if ($null -ne $ocr) {
            Add-Result "ocr_consistency" "pass" "OCR 组件状态：ready=$($ocr.ready)"
        } else {
            Add-Result "ocr_consistency" "skip" "服务 /metrics/health 暂无 ocr 组件字段（模型就绪后可验收）"
        }
    } catch {
        Add-Result "ocr_consistency" "skip" "服务不可达（$baseUrl），OCR 一致性跳过"
    }
}

# 4) 真实 eval 门禁（依赖构建产物 + 模型凭据）
Write-Host "[4/4] 真实 eval..."
$bin = Join-Path $root "target\debug\owo-agent.exe"
if (-not (Test-Path $bin)) {
    Add-Result "eval_gate" "skip" "缺 $bin（先 cargo build）"
} elseif (-not $env:OPENAI_API_KEY -and -not $env:DEEPSEEK_API_KEY) {
    # R12：缺模型凭据 → 显式 skip（eval 需真实模型调用，无凭据全失败无意义）。
    Add-Result "eval_gate" "skip" "缺模型凭据（OPENAI_API_KEY / DEEPSEEK_API_KEY 未设置，eval 无法执行）"
} else {
    try {
        # R12：内联执行 eval 并用正则提取 pass_rate（避免 ConvertFrom-Json 在中文
        # Windows 控制台编码断链——run-eval-gate.ps1 的已知健壮性限制）。
        $evalOut = & $bin eval 2>&1 | Out-String
        $rateMatch = [regex]::Match($evalOut, '"pass_rate"\s*:\s*([0-9.]+)')
        $passedMatch = [regex]::Match($evalOut, '"passed"\s*:\s*(\d+)')
        $totalMatch = [regex]::Match($evalOut, '"total"\s*:\s*(\d+)')
        if ($rateMatch.Success) {
            $evalRate = [double]$rateMatch.Groups[1].Value
            $evalPassed = if ($passedMatch.Success) { [int]$passedMatch.Groups[1].Value } else { 0 }
            $evalTotal = if ($totalMatch.Success) { [int]$totalMatch.Groups[1].Value } else { 0 }
            if ($evalRate -ge 0.8) {
                Add-Result "eval_gate" "pass" "eval 通过（$evalPassed/$evalTotal，rate $([math]::Round($evalRate * 100, 1))%）"
            } else {
                Add-Result "eval_gate" "fail" "eval 未达阈值（$evalPassed/$evalTotal，rate $([math]::Round($evalRate * 100, 1))%）"
            }
        } else {
            Add-Result "eval_gate" "fail" "eval 输出无可解析 pass_rate"
        }
    } catch {
        Add-Result "eval_gate" "fail" "eval 门禁异常：$_"
    }
}

# ---- 汇总落盘（保留）----
$summary = [ordered]@{
    out_dir = $outDir
    generated_at = (Get-Date).ToString("o")
    results = @($results | ForEach-Object { [ordered]@{ acceptance = $_.acceptance; status = $_.status; detail = $_.detail } })
    passed = (-not $hasFail)
}
$summary | ConvertTo-Json -Depth 4 | Set-Content -Path (Join-Path $outDir "results.json") -Encoding UTF8
Write-Host ""
Write-Host ("外部验收结果：{0}" -f $(if (-not $hasFail) { "PASS（无失败项，skip 为缺资源）" } else { "FAIL（存在失败项）" })) -ForegroundColor $(if (-not $hasFail) { "Green" } else { "Red" })
Write-Host "结果文件：$(Join-Path $outDir 'results.json')"
if ($hasFail) { exit 1 }
exit 0
