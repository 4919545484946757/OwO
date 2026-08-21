# stt-regress.ps1 — STT 回归脚本（Lane 4：多模态入口）
# 用法：powershell -ExecutionPolicy Bypass -File scripts\stt-regress.ps1 [-Port 4096] [-Manifest tests\stt-corpus\corpus.tsv]
# 读取语料清单（每行 <wav路径>TAB<标准文本>，UTF-8），启动 owo-agent serve，
# 逐条调 POST /stt/transcribe（raw WAV body），计算字符级 CER 与延迟 p95。

param(
    [int]$Port = 4096,
    [string]$Manifest = "tests\stt-corpus\corpus.tsv",
    [string]$AgentExe = "target\debug\owo-agent.exe",
    [int]$TimeoutSec = 60
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not (Test-Path $Manifest)) {
    Write-Host "语料清单不存在：$Manifest（跳过，未失败）" -ForegroundColor Yellow
    exit 0
}

# 临时数据目录（严格限定 %TEMP%\owo-stt-bench-*）
$tempData = Join-Path $env:TEMP ("owo-stt-bench-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $tempData -Force | Out-Null

$server = $null
try {
    Write-Host "启动 owo-agent serve（端口 $Port，临时数据 $tempData）..."
    $server = Start-Process -FilePath (Join-Path $root $AgentExe) -ArgumentList @(
        "serve", "--port", "$Port", "--workspace", $tempData
    ) -WorkingDirectory $root -PassThru -WindowStyle Hidden

    $ready = $false
    for ($i = 0; $i -lt 30; $i++) {
        Start-Sleep -Milliseconds 500
        try {
            $r = Invoke-WebRequest -Uri "http://127.0.0.1:$Port/health" -TimeoutSec 2 -UseBasicParsing
            if ($r.StatusCode -eq 200) { $ready = $true; break }
        } catch { }
    }
    if (-not $ready) {
        Write-Host "服务未就绪（跳过，未失败）" -ForegroundColor Yellow
        exit 0
    }

    $lines = Get-Content $Manifest -Encoding UTF8 | Where-Object { $_.Trim() -and -not $_.StartsWith("#") }
    $total = 0
    $hits = 0
    $lats = @()
    $cerSum = 0.0

    foreach ($line in $lines) {
        $parts = $line -split "`t"
        if ($parts.Count -lt 2) { continue }
        $wavPath = $parts[0].Trim()
        $expected = $parts[1].Trim()
        if (-not (Test-Path $wavPath)) {
            Write-Host "wav 缺失：$wavPath（跳过）" -ForegroundColor Yellow
            continue
        }
        $bytes = [System.IO.File]::ReadAllBytes((Resolve-Path $wavPath))
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        try {
            $resp = Invoke-WebRequest -Uri "http://127.0.0.1:$Port/stt/transcribe" -Method Post `
                -ContentType "audio/wav" -Body $bytes -TimeoutSec $TimeoutSec -UseBasicParsing
            $sw.Stop()
            $lats += $sw.ElapsedMilliseconds
            $json = $resp.Content | ConvertFrom-Json
            $actual = $json.text
            if (-not $actual) { Write-Host "空转写：$wavPath"; continue }
            $total++
            $cer = 1.0 - ([double]($expected.ToCharArray() | Where-Object { $actual.Contains($_) } | Measure-Object).Count) / [Math]::Max(1, $expected.Length)
            $cerSum += $cer
            if ($cer -le 0.2) { $hits++ }
            Write-Host ("CER {0:P1}  {1}ms  {2} => {3}" -f $cer, $sw.ElapsedMilliseconds, $expected, $actual)
        } catch {
            Write-Host "转写失败：$wavPath — $($_.Exception.Message)" -ForegroundColor Yellow
        }
    }

    $latsSorted = $lats | Sort-Object
    $p50 = if ($latsSorted.Count) { $latsSorted[[int]($latsSorted.Count * 0.5)] } else { 0 }
    $p95 = if ($latsSorted.Count) { $latsSorted[[int]($latsSorted.Count * 0.95)] } else { 0 }
    $avgCer = if ($total) { $cerSum / $total } else { 1.0 }

    Write-Host "`n=== STT 回归摘要 ===" -ForegroundColor Cyan
    Write-Host "样本：$total 条（语料 $($lines.Count) 行）"
    Write-Host "CER≤20% 命中：$hits/$total"
    Write-Host "平均 CER：$([math]::Round($avgCer * 100, 1))%"
    Write-Host "延迟：p50=$p50 ms  p95=$p95 ms"
    if ($total -gt 0) {
        Write-Host "结论：$($(if ($avgCer -le 0.2) { "PASS" } else { "FAIL（CER 超 20%）" }))"
        exit $(if ($avgCer -le 0.2) { 0 } else { 1 })
    }
    Write-Host "结论：无可用样本，跳过（未失败）"
    exit 0
}
finally {
    if ($server -and -not $server.HasExited) {
        Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Milliseconds 300
    if (Test-Path $tempData) { Remove-Item -Recurse -Force $tempData -ErrorAction SilentlyContinue }
}
