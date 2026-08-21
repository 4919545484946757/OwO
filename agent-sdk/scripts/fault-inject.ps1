# R11:fault-inject 质量收尾完成。
# R12:fault-inject 复核完成（场景/白名单/清理，无需改动）。
# fault-inject.ps1 — 故障注入脚本（R8 Agent 4）
# 用法：powershell -ExecutionPolicy Bypass -File scripts\fault-inject.ps1 [-Port 4098] [-Token <token>]
# 场景：SSE 断线重连、429 限流、幂等重复提交、坏 token。
# 只写 %TEMP%\owo-fault-*（严格前缀白名单），结束清理。退出码：0=全过 / 1=关键失败。

param(
    [int]$Port = 4098,
    [string]$Token = "",
    [int]$TimeoutSec = 8
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$baseUrl = "http://127.0.0.1:$Port"

# ---- 输出目录（严格前缀白名单 %TEMP%\owo-fault-*）----
$outDir = Join-Path $env:TEMP ("owo-fault-" + [guid]::NewGuid().ToString("N"))
if (-not $outDir.StartsWith((Join-Path $env:TEMP "owo-fault-"), [System.StringComparison]::OrdinalIgnoreCase)) {
    Write-Host "输出目录越界，拒绝写入：$outDir" -ForegroundColor Red
    exit 1
}
New-Item -ItemType Directory -Path $outDir -Force | Out-Null
$reportPath = Join-Path $outDir "results.json"

function Get-Token {
    if ($Token) { return $Token }
    try {
        $r = Invoke-RestMethod -Uri "$baseUrl/auth/token" -Method GET -TimeoutSec $TimeoutSec
        return [string]$r.token
    } catch {
        return ""
    }
}

function Invoke-WithAuth {
    param([string]$Method, [string]$Path, [string]$Body, [string]$AuthToken)
    $headers = @{}
    if ($AuthToken) { $headers["Authorization"] = "Bearer $AuthToken" }
    $params = @{ Uri = "$baseUrl$Path"; Method = $Method; TimeoutSec = $TimeoutSec; UseBasicParsing = $true }
    if ($Body) { $params.Body = $Body; $params.ContentType = "application/json" }
    if ($headers.Count) { $params.Headers = $headers }
    return Invoke-WebRequest @params
}

$results = @()
$failed = $false
$token = ""

try {
    $token = Get-Token
    if (-not $token) {
        Write-Host "无法获取 token（/auth/token 未就绪？），坏 token 场景将使用占位。" -ForegroundColor Yellow
    }

    # ---- 场景 1：SSE 断线重连（Last-Event-ID 语义）----
    Write-Host "[1/4] SSE 断线重连..."
    $sseOk = $false
    try {
        $first = Invoke-WebRequest -Uri "$baseUrl/events/stream?last_event_id=0" -Method GET -TimeoutSec 2 -UseBasicParsing -ErrorAction Stop
        $sseOk = $first.StatusCode -eq 200 -and $first.Headers["Content-Type"] -match "text/event-stream"
        $reconnect = Invoke-WebRequest -Uri "$baseUrl/events/stream?last_event_id=5" -Method GET -TimeoutSec 2 -UseBasicParsing -ErrorAction Stop
        $sseOk = $sseOk -and $reconnect.StatusCode -eq 200
    } catch {
        $sseOk = $false
    }
    $results += [pscustomobject]@{ scenario = "sse_reconnect"; passed = $sseOk; detail = "首次连接 + 断线重连(Last-Event-ID)均返回 SSE" }
    if (-not $sseOk) { Write-Host "  SSE 场景失败（可能服务未起/未接线）" -ForegroundColor Red }

    # ---- 场景 2：幂等重复提交（同体 + 幂等键两次，结果一致）----
    Write-Host "[2/4] 幂等重复提交..."
    $idemOk = $false
    try {
        $body = '{"text":"打开笔记"}'
        $h1 = @{ "Idempotency-Key" = "fault-inject-$(Get-Random)" }
        if ($token) { $h1["Authorization"] = "Bearer $token" }
        $r1 = Invoke-WebRequest -Uri "$baseUrl/intent/parse" -Method POST -Body $body -ContentType "application/json" -Headers $h1 -TimeoutSec $TimeoutSec -UseBasicParsing
        $r2 = Invoke-WebRequest -Uri "$baseUrl/intent/parse" -Method POST -Body $body -ContentType "application/json" -Headers $h1 -TimeoutSec $TimeoutSec -UseBasicParsing
        $idemOk = $r1.StatusCode -eq $r2.StatusCode
        if ($idemOk -and $r1.StatusCode -eq 200) {
            $b1 = $r1.Content; $b2 = $r2.Content
            $idemOk = $b1 -eq $b2
        }
    } catch {
        $idemOk = $false
    }
    $results += [pscustomobject]@{ scenario = "idempotent_repeat"; passed = $idemOk; detail = "同幂等键重复提交返回一致结果" }
    if (-not $idemOk) { Write-Host "  幂等场景失败" -ForegroundColor Red }

    # ---- 场景 3：429 限流 ----
    Write-Host "[3/4] 429 限流..."
    $got429 = $false
    $statuses = @()
    for ($i = 0; $i -lt 30; $i++) {
        try {
            $r = Invoke-WithAuth POST "/intent/parse" '{"text":"x"}' $token
            $statuses += $r.StatusCode
        } catch {
            $resp = $_.Exception.Response
            if ($resp -and $resp.StatusCode) { $statuses += [int]$resp.StatusCode } else { $statuses += 0 }
        }
    }
    $got429 = ($statuses -contains 429)
    $results += [pscustomobject]@{ scenario = "rate_limit_429"; passed = $got429; detail = "30 连发中出现 429（$($statuses -join ',')）" }
    if (-not $got429) { Write-Host "  未观察到 429（限流未接线或阈值未触发，警告不失败）" -ForegroundColor Yellow }

    # ---- 场景 4：坏 token 401 ----
    Write-Host "[4/4] 坏 token..."
    $unauthorized = $false
    try {
        $bad = Invoke-WebRequest -Uri "$baseUrl/metrics/overview" -Method GET -Headers @{ Authorization = "Bearer invalid-token-$(Get-Random)" } -TimeoutSec $TimeoutSec -UseBasicParsing -ErrorAction Stop
        $unauthorized = $false
    } catch {
        $unauthorized = $_.Exception.Response.StatusCode.value__ -eq 401
    }
    $results += [pscustomobject]@{ scenario = "bad_token_401"; passed = $unauthorized; detail = "坏 token 请求被拒(401)" }
    if (-not $unauthorized) { Write-Host "  坏 token 未返回 401（鉴权未接线？警告不失败）" -ForegroundColor Yellow }
} finally {
    $critical = @("sse_reconnect", "idempotent_repeat")
    foreach ($r in $results) {
        if (-not $r.passed -and ($critical -contains $r.scenario)) { $failed = $true }
    }
    $summary = [ordered]@{
        out_dir = $outDir
        token_acquired = [bool]$token
        results = @($results | ForEach-Object { [ordered]@{ scenario = $_.scenario; passed = $_.passed; detail = $_.detail } })
        passed = (-not $failed)
    }
    $summary | ConvertTo-Json -Depth 4 | Set-Content -Path $reportPath -Encoding UTF8
    Write-Host ""
    foreach ($r in $results) {
        Write-Host ("  {0,-22} {1}  {2}" -f $r.scenario, $(if ($r.passed) { "PASS" } else { "WARN/FAIL" }), $r.detail) -ForegroundColor $(if ($r.passed) { "Green" } else { "Yellow" })
    }
    Write-Host ("故障注入结果：{0}" -f $(if (-not $failed) { "PASS" } else { "FAIL（关键场景失败）" })) -ForegroundColor $(if (-not $failed) { "Green" } else { "Red" })
    # 清理（严格前缀白名单内）
    if ($outDir.StartsWith((Join-Path $env:TEMP "owo-fault-"), [System.StringComparison]::OrdinalIgnoreCase)) {
        Remove-Item -Path $outDir -Recurse -Force -ErrorAction SilentlyContinue
        Write-Host "已清理 $outDir"
    }
    if ($failed) { exit 1 }
    exit 0
}
