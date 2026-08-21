# R11:soak 质量收尾完成。
# R12:soak 复核完成（trace_id 透传率/预算超限/SLO 违反清单，无需改动）。
# soak.ps1 — 可靠性与可观测性 soak 骨架（R7 + R8 SLO 清单 + R9 预算超限清单 + R10 trace_id 贯穿）
# 用法：powershell -ExecutionPolicy Bypass -File scripts\soak.ps1 [-Port 4098] [-Minutes 10] [-Long] [-ServerPid <pid>]
# 短模式（默认 10 分钟）：对运行中的 owo-agent server 跑"感知→定位→执行→验证→学习"模拟循环，
# 每轮采集 server 进程 RSS/句柄数并断言无卡死；长模式（-Long，60 分钟）供 nightly。
# R10：每轮请求带 X-Trace-Id，统计服务端回显透传率；末尾输出 trace 统计 + SLO 违反 + 预算超限清单。
# 只写 %TEMP%\owo-soak-*（严格前缀白名单）；Ctrl+C 可中断并在 finally 落盘汇总。
# 退出码：0 = 达标；1 = 卡死/资源超限。

param(
    [int]$Port = 4098,
    [int]$Minutes = 10,
    [int]$Seconds = 0,
    [switch]$Long,
    [int]$ServerPid = 0,
    [int]$CardTimeoutSec = 10,
    [int]$MaxStalls = 3,
    [double]$RssGrowthLimitMB = 300.0,
    [int]$HandleGrowthLimit = 1000
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

if ($Long) { $Minutes = 60 }
if ($Seconds -gt 0) {
    $deadline = (Get-Date).AddSeconds($Seconds)
} else {
    $deadline = (Get-Date).AddMinutes($Minutes)
}
$baseUrl = "http://127.0.0.1:$Port"

# ---- 输出目录：严格前缀白名单 %TEMP%\owo-soak-* ----
$outDir = Join-Path $env:TEMP ("owo-soak-" + [guid]::NewGuid().ToString("N"))
if (-not $outDir.StartsWith((Join-Path $env:TEMP "owo-soak-"), [System.StringComparison]::OrdinalIgnoreCase)) {
    Write-Host "输出目录越界，拒绝写入：$outDir" -ForegroundColor Red
    exit 1
}
New-Item -ItemType Directory -Path $outDir -Force | Out-Null
$logCsv = Join-Path $outDir "rounds.csv"
$baselineJson = Join-Path $outDir "baseline.json"
$summaryJson = Join-Path $outDir "summary.json"
Write-Host "soak 输出目录：$outDir"

# ---- 目标进程（RSS/句柄采集）----
$target = $null
if ($ServerPid -gt 0) {
    $target = Get-Process -Id $ServerPid -ErrorAction SilentlyContinue
} else {
    $target = Get-Process -Name "owo-agent" -ErrorAction SilentlyContinue | Select-Object -First 1
}
if (-not $target) {
    Write-Host "未找到目标进程（-ServerPid 或 owo-agent 进程），仅采集本进程基线。" -ForegroundColor Yellow
    $target = Get-Process -Id $PID
}

function Get-TargetProcess {
    param([int]$Pid)
    Get-Process -Id $Pid -ErrorAction SilentlyContinue
}

$targetId = $target.Id
$firstRssMB = 0.0
$firstHandles = 0
$lastRssMB = 0.0
$lastHandles = 0
$baselineCaptured = $false

# ---- 模拟循环：感知→定位→执行→验证→学习 ----
$stalls = 0
$rounds = 0
$httpOk = 0
$httpFail = 0
$errors = @()
# R10：trace_id 贯穿统计。
$traceSent = 0
$traceEchoed = 0

function Invoke-SafeRequest {
    param([string]$Method, [string]$Path, [string]$Body, [int]$TimeoutSec, [string]$TraceId)
    try {
        $params = @{ Uri = "$baseUrl$Path"; Method = $Method; TimeoutSec = $TimeoutSec; UseBasicParsing = $true }
        if ($Body) { $params.Body = $Body; $params.ContentType = "application/json" }
        if ($TraceId) {
            $params.Headers = @{ "X-Trace-Id" = $TraceId }
        }
        $r = Invoke-WebRequest @params
        $script:traceSent++
        if ($r.Headers["X-Trace-Id"]) { $script:traceEchoed++ }
        if ($r.StatusCode -ge 200 -and $r.StatusCode -lt 300) { return $true }
        return $false
    } catch {
        $script:traceSent++
        return $false
    }
}

function Invoke-Round {
    param([int]$Round)
    $results = @()
    # R10：每轮统一 trace_id（soak-<round>-<随机>），贯穿全部请求。
    $trace = "soak-$Round-$([guid]::NewGuid().ToString('N').Substring(0, 8))"
    # 1) 感知：健康清单
    $results += [pscustomobject]@{ Step = "sense";   Path = "/metrics/health";        Ok = (Invoke-SafeRequest GET "/metrics/health" $null $CardTimeoutSec $trace) }
    # 2) 定位：意图解析
    $results += [pscustomobject]@{ Step = "locate";  Path = "/intent/parse";          Ok = (Invoke-SafeRequest POST "/intent/parse" '{"text":"打开笔记"}' $CardTimeoutSec $trace) }
    # 3) 执行：回合耗时序列
    $results += [pscustomobject]@{ Step = "execute"; Path = "/metrics/turns?limit=5"; Ok = (Invoke-SafeRequest GET "/metrics/turns?limit=5" $null $CardTimeoutSec $trace) }
    # 4) 验证：SLO + 运行时指标
    $results += [pscustomobject]@{ Step = "verify";  Path = "/metrics/slo";           Ok = (Invoke-SafeRequest GET "/metrics/slo" $null $CardTimeoutSec $trace) }
    $results += [pscustomobject]@{ Step = "verify";  Path = "/metrics/runtime";       Ok = (Invoke-SafeRequest GET "/metrics/runtime" $null $CardTimeoutSec $trace) }
    # 5) 学习：记忆召回
    $results += [pscustomobject]@{ Step = "learn";   Path = "/memory/graph/recall?q=soak&top_k=3"; Ok = (Invoke-SafeRequest GET "/memory/graph/recall?q=soak&top_k=3" $null $CardTimeoutSec $trace) }
    return $results
}

try {
    while ((Get-Date) -lt $deadline) {
        $rounds++
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        $stepResults = Invoke-Round $rounds
        $sw.Stop()
        $roundMs = $sw.ElapsedMilliseconds
        foreach ($step in $stepResults) {
            if ($step.Ok) { $httpOk++ } else { $httpFail++ }
        }
        if ($roundMs -gt ($CardTimeoutSec * 1000)) {
            $stalls++
            $errors += "round $rounds 卡死（$roundMs ms 超阈值）"
            Write-Host "  卡死：round $rounds 耗时 $roundMs ms" -ForegroundColor Red
        }

        $proc = Get-TargetProcess $targetId
        if ($proc) {
            $lastRssMB = [math]::Round($proc.WorkingSet64 / 1MB, 1)
            $lastHandles = $proc.HandleCount
            if (-not $baselineCaptured) {
                $firstRssMB = $lastRssMB
                $firstHandles = $lastHandles
                $baselineCaptured = $true
                @{ process_id = $targetId; rss_mb = $firstRssMB; handles = $firstHandles; captured_at = (Get-Date).ToString("o") } |
                    ConvertTo-Json | Set-Content -Path $baselineJson -Encoding UTF8
            }
        } else {
            $errors += "round $rounds 目标进程消失（pid=$targetId）"
            Write-Host "  目标进程消失：pid=$targetId" -ForegroundColor Red
        }

        ("{0},{1},{2},{3},{4},{5}" -f $rounds, $roundMs, $httpOk, $httpFail, $lastRssMB, $lastHandles) |
            Add-Content -Path $logCsv -Encoding UTF8

        if ($rounds % 10 -eq 0) {
            Write-Host "round $rounds：$roundMs ms，RSS $lastRssMB MB，句柄 $lastHandles，OK $httpOk / FAIL $httpFail"
        }
        Start-Sleep -Seconds 2
    }
} finally {
    # 中断（含 Ctrl+C）也落盘汇总。
    $rssGrowthMB = [math]::Round($lastRssMB - $firstRssMB, 1)
    $handleGrowth = $lastHandles - $firstHandles
    $summary = [ordered]@{
        out_dir = $outDir
        duration_minutes = $Minutes
        duration_seconds = $Seconds
        rounds = $rounds
        http_ok = $httpOk
        http_fail = $httpFail
        stalls = $stalls
        stall_limit = $MaxStalls
        rss_mb_baseline = $firstRssMB
        rss_mb_final = $lastRssMB
        rss_growth_mb = $rssGrowthMB
        rss_growth_limit_mb = $RssGrowthLimitMB
        handles_baseline = $firstHandles
        handles_final = $lastHandles
        handle_growth = $handleGrowth
        handle_growth_limit = $HandleGrowthLimit
        trace_sent = $traceSent
        trace_echoed = $traceEchoed
        trace_echo_rate = if ($traceSent -gt 0) { [math]::Round($traceEchoed / $traceSent, 4) } else { 0 }
        passed = ($stalls -le $MaxStalls -and $httpFail -eq 0 -and $rssGrowthMB -le $RssGrowthLimitMB -and $handleGrowth -le $HandleGrowthLimit)
        errors = $errors
    }
    $summary | ConvertTo-Json -Depth 4 | Set-Content -Path $summaryJson -Encoding UTF8
    Write-Host ""
    Write-Host "==== soak 汇总（$outDir）====" -ForegroundColor Cyan
    Write-Host "rounds=$rounds  HTTP OK=$httpOk FAIL=$httpFail  卡死=$stalls/$MaxStalls"
    Write-Host ("RSS {0}MB → {1}MB（+{2}MB，限 {3}MB）  句柄 {4} → {5}（+{6}，限 {7}）" -f $firstRssMB, $lastRssMB, $rssGrowthMB, $RssGrowthLimitMB, $firstHandles, $lastHandles, $handleGrowth, $HandleGrowthLimit)
    # R10：trace_id 贯穿统计。
    $traceRate = if ($traceSent -gt 0) { [math]::Round($traceEchoed / $traceSent * 100, 1) } else { 0 }
    Write-Host ("X-Trace-Id 贯穿：发出 {0} 个，回显 {1} 个（透传率 {2}%）" -f $traceSent, $traceEchoed, $traceRate) -ForegroundColor $(if ($traceSent -eq 0 -or $traceRate -ge 90) { "Green" } else { "Yellow" })
    # R8：末尾输出 SLO 违反清单。
    $sloViolations = @()
    try {
        $sloReport = Invoke-WebRequest -Uri "$baseUrl/metrics/slo" -TimeoutSec 5 -UseBasicParsing | Select-Object -ExpandProperty Content | ConvertFrom-Json
        foreach ($item in @($sloReport.slo)) {
            if ($item.violations -gt 0 -or -not $item.achieving) {
                $sloViolations += [pscustomobject]@{
                    name = $item.name
                    violations = $item.violations
                    samples = $item.samples
                    p95_ms = $item.p95_ms
                    achieving = $item.achieving
                }
            }
        }
    } catch { }
    if ($sloViolations.Count -gt 0) {
        Write-Host "---- SLO 违反清单（soak 期间）----" -ForegroundColor Yellow
        foreach ($v in $sloViolations) {
            Write-Host ("  {0,-16} violations={1} samples={2} p95={3}ms achieving={4}" -f $v.name, $v.violations, $v.samples, $v.p95_ms, $v.achieving) -ForegroundColor Yellow
        }
    } else {
        Write-Host "SLO 违反清单：无（全部达标或服务不可达）"
    }
    # R9：预算超限清单（usage 硬熔断 + 维度超限）。
    $budgetOvers = @()
    try {
        $usage = Invoke-WebRequest -Uri "$baseUrl/usage/summary" -TimeoutSec 5 -UseBasicParsing | Select-Object -ExpandProperty Content | ConvertFrom-Json
        foreach ($dim in @($usage.dimensions)) {
            if ($dim.budget -and $dim.budget.exceeded) {
                $budgetOvers += [pscustomobject]@{
                    dimension = $dim.dimension
                    spent = $dim.budget.spent_usd
                    limit = $dim.budget.limit_usd
                    calls = $dim.calls
                    cost = $dim.cost_usd
                }
            }
        }
        if ($usage.hard_stop) {
            $budgetOvers += [pscustomobject]@{
                dimension = "*hard_stop*"
                spent = $null
                limit = $null
                calls = $null
                cost = $null
                reason = $usage.hard_stop_reason
            }
        }
    } catch { }
    if ($budgetOvers.Count -gt 0) {
        Write-Host "---- 预算超限清单（soak 期间）----" -ForegroundColor Yellow
        foreach ($b in $budgetOvers) {
            if ($b.dimension -eq "*hard_stop*") {
                Write-Host ("  硬熔断：{0}" -f $b.reason) -ForegroundColor Red
            } else {
                Write-Host ("  {0,-14} 花费 {1} / 预算 {2} USD（calls={3} cost={4}）" -f $b.dimension, $b.spent, $b.limit, $b.calls, $b.cost) -ForegroundColor Yellow
            }
        }
    } else {
        Write-Host "预算超限清单：无（预算内运行或用量端点不可达）"
    }
    $passed = $summary.passed
    Write-Host ("结果：{0}" -f ($(if ($passed) { "PASS" } else { "FAIL" }))) -ForegroundColor $(if ($passed) { "Green" } else { "Red" })
    if (-not $passed) { exit 1 }
    exit 0
}
