# bench-metrics.ps1 — R5 可观测性性能护栏（R5 Agent 3 交付）
# 自包含：随机端口启动 owo-agent serve（隐藏窗口）→ 循环 N 次 GET /health 与 /metrics/overview
# → 输出 p50/p95/错误率 → 结束清理进程与临时数据目录（严格限定 %TEMP%\owo-bench-*）。
param(
    [int]$Iterations = 200,
    [int]$Port = 0,           # 0 = 随机
    [int]$TimeoutMs = 5000
)

$ErrorActionPreference = "Stop"
$env:PATH = "C:\Users\23843\.cargo\bin;" + $env:PATH
$root = Split-Path -Parent $PSScriptRoot   # agent-sdk/

if ($Port -eq 0) { $Port = Get-Random -Minimum 12000 -Maximum 32000 }
$tempRoot = Join-Path $env:TEMP "owo-bench-$Port"
New-Item -ItemType Directory -Path $tempRoot -Force | Out-Null
$workspace = Join-Path $tempRoot "ws"
New-Item -ItemType Directory -Path $workspace -Force | Out-Null
$dataDir = Join-Path $tempRoot "data"
New-Item -ItemType Directory -Path $dataDir -Force | Out-Null

Write-Host "==> 启动服务：port=$Port data=$dataDir workspace=$workspace"
$proc = Start-Process -FilePath "cargo" -ArgumentList @("run", "-q", "-p", "owo-agent-cli", "--", "serve", "--workspace", $workspace, "--port", "$Port") `
    -WorkingDirectory $root -WindowStyle Hidden -PassThru

try {
    $base = "http://127.0.0.1:$Port"
    $up = $false
    for ($i = 0; $i -lt 120; $i++) {
        Start-Sleep -Milliseconds 500
        try {
            $r = Invoke-WebRequest -Uri "$base/health" -UseBasicParsing -TimeoutSec 2
            if ($r.StatusCode -eq 200) { $up = $true; break }
        } catch { }
    }
    if (-not $up) { throw "服务 $base 120 次探测未就绪（首次编译可能较慢，可重试）" }

    $latencies = New-Object System.Collections.Generic.List[double]
    $errors = 0
    for ($i = 0; $i -lt $Iterations; $i++) {
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        try {
            $r = Invoke-WebRequest -Uri "$base/metrics/overview" -UseBasicParsing -TimeoutSec 5
            $sw.Stop()
            if ($r.StatusCode -ne 200) { $errors++ }
        } catch {
            $sw.Stop()
            $errors++
        }
        $latencies.Add($sw.Elapsed.TotalMilliseconds)
    }

    $sorted = $latencies | Sort-Object
    $count = $sorted.Count
    $p50 = if ($count -eq 0) { 0 } else { $sorted[[math]::Floor($count * 0.5) - 1] }
    $p95 = if ($count -eq 0) { 0 } else { $sorted[[math]::Floor($count * 0.95) - 1] }
    $avg = if ($count -eq 0) { 0 } else { ($sorted | Measure-Object -Average).Average }
    $errRate = if ($count -eq 0) { 1 } else { $errors / $count }

    Write-Host ""
    Write-Host "==== 基准结果（$count 次，/metrics/overview）===="
    Write-Host "  avg : $([math]::Round($avg, 2)) ms"
    Write-Host "  p50 : $([math]::Round($p50, 2)) ms"
    Write-Host "  p95 : $([math]::Round($p95, 2)) ms"
    Write-Host "  errors: $errors / $count（$([math]::Round($errRate * 100, 3))%）"
    Write-Host "  port: $Port"
    Write-Host "  temp: $tempRoot"

    if ($errRate -gt 0.05) {
        Write-Host "错误率超过 5%，判定失败" -ForegroundColor Red
        exit 1
    }
    Write-Host "通过" -ForegroundColor Green
} finally {
    if ($proc -and -not $proc.HasExited) {
        Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 500
    }
    # 清理：仅限 %TEMP%\owo-bench-*（严格白名单）
    if ($tempRoot.StartsWith((Join-Path $env:TEMP "owo-bench-"))) {
        Remove-Item -Path $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
        Write-Host "==> 已清理 $tempRoot"
    }
}
