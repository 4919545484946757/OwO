# R11:diagnose 质量收尾完成。
# R12:diagnose 完成（SLO/usage 摘要 summary.txt）。
# diagnose.ps1 — 诊断包收集脚本（R8 + R9 + R10：SLO 周报/遥测状态 + R12：摘要）
# 用法：powershell -ExecutionPolicy Bypass -File scripts\diagnose.ps1 [-Port 4098] [-Token ""]
# 收集：版本/健康/日志摘要/最近 trace 元数据（脱敏）/SLO/用量/运行时指标/
# SLO 告警/周期报表/周报/遥测状态，打包到 %TEMP%\owo-diagnose-*（严格前缀白名单，诊断包保留不清理）。

param(
    [int]$Port = 4098,
    [string]$Token = "",
    [int]$TimeoutSec = 8
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$baseUrl = "http://127.0.0.1:$Port"

# ---- 输出目录（严格前缀白名单 %TEMP%\owo-diagnose-*）----
$outDir = Join-Path $env:TEMP ("owo-diagnose-" + [guid]::NewGuid().ToString("N"))
if (-not $outDir.StartsWith((Join-Path $env:TEMP "owo-diagnose-"), [System.StringComparison]::OrdinalIgnoreCase)) {
    Write-Host "输出目录越界，拒绝写入：$outDir" -ForegroundColor Red
    exit 1
}
New-Item -ItemType Directory -Path $outDir -Force | Out-Null
$collectDir = Join-Path $outDir "collect"
New-Item -ItemType Directory -Path $collectDir -Force | Out-Null

function Get-Json {
    param([string]$Path)
    try {
        $headers = @{}
        if ($Token) { $headers["Authorization"] = "Bearer $Token" }
        $r = Invoke-WebRequest -Uri "$baseUrl$Path" -Method GET -Headers $headers -TimeoutSec $TimeoutSec -UseBasicParsing
        return $r.Content
    } catch {
        return '{"error": "request failed"}'
    }
}

# 脱敏：删除 prompt/消息类字段（诊断包不落详文）。
function Get-RedactedTurns {
    param([string]$Raw)
    try {
        $obj = $Raw | ConvertFrom-Json
        foreach ($turn in @($obj.turns)) {
            $turn.PSObject.Properties.Remove("prompt")
        }
        return ($obj | ConvertTo-Json -Depth 6 -Compress)
    } catch {
        return '{"error": "parse failed"}'
    }
}

try {
    # 1) 版本与环境
    $envInfo = [ordered]@{
        os = [System.Environment]::OSVersion.VersionString
        ps_version = $PSVersionTable.PSVersion.ToString()
        port = $Port
        collected_at = (Get-Date).ToString("o")
        agent_version = ""
    }
    $agentExe = Join-Path $root "target\debug\owo-agent.exe"
    if (Test-Path $agentExe) {
        try { $envInfo.agent_version = (& $agentExe --version 2>&1 | Select-Object -First 1) } catch { }
    }
    $envInfo | ConvertTo-Json | Set-Content -Path (Join-Path $collectDir "environment.json") -Encoding UTF8

    # 2) 健康与指标（脱敏由服务端承担；turns 的 prompt 二次脱敏）
    Get-Json "/metrics/health"   | Set-Content -Path (Join-Path $collectDir "health.json") -Encoding UTF8
    Get-Json "/metrics/overview" | Set-Content -Path (Join-Path $collectDir "overview.json") -Encoding UTF8
    Get-Json "/metrics/runtime"  | Set-Content -Path (Join-Path $collectDir "runtime.json") -Encoding UTF8
    Get-Json "/metrics/slo"      | Set-Content -Path (Join-Path $collectDir "slo.json") -Encoding UTF8
    Get-Json "/usage/summary"    | Set-Content -Path (Join-Path $collectDir "usage.json") -Encoding UTF8
    Get-RedactedTurns (Get-Json "/metrics/turns?limit=10") | Set-Content -Path (Join-Path $collectDir "recent_traces.json") -Encoding UTF8

    # 2b) R9：SLO 告警（规则 + 最近事件）与用量周期报表
    Get-Json "/metrics/slo/alerts"   | Set-Content -Path (Join-Path $collectDir "slo_alerts.json") -Encoding UTF8
    Get-Json "/usage/report?days=7"  | Set-Content -Path (Join-Path $collectDir "usage_report.json") -Encoding UTF8

    # 2c) R10：SLO 周报（7 天周期聚合）与可选遥测状态
    Get-Json "/metrics/slo/report?days=7" | Set-Content -Path (Join-Path $collectDir "slo_weekly.json") -Encoding UTF8
    Get-Json "/metrics/telemetry/status"  | Set-Content -Path (Join-Path $collectDir "telemetry.json") -Encoding UTF8

    # 3) 日志摘要：尝试常见日志目录（data_root/logs、%TEMP%\owo-*）
    $logCandidates = @(
        (Join-Path $env:TEMP "owo-agent\logs"),
        (Join-Path $root "data\logs"),
        (Join-Path $env:LOCALAPPDATA "owo-agent\logs")
    )
    $logSummary = @()
    foreach ($candidate in $logCandidates) {
        if (Test-Path $candidate) {
            $logSummary += Get-ChildItem $candidate -File -ErrorAction SilentlyContinue | Select-Object -First 5 |
                ForEach-Object { [ordered]@{ dir = $candidate; name = $_.Name; bytes = $_.Length; last_write = $_.LastWriteTime.ToString("o") } }
        }
    }
    @{ count = $logSummary.Count; entries = $logSummary } | ConvertTo-Json -Depth 4 |
        Set-Content -Path (Join-Path $collectDir "logs_summary.json") -Encoding UTF8

    # 3b) R12：SLO/usage/遥测人类可读摘要（诊断包首屏，非原始 JSON）。
    try {
        $sloObj = (Get-Content -Path (Join-Path $collectDir "slo.json") -Raw -Encoding UTF8) | ConvertFrom-Json
        $usageObj = (Get-Content -Path (Join-Path $collectDir "usage.json") -Raw -Encoding UTF8) | ConvertFrom-Json
        $teleObj = (Get-Content -Path (Join-Path $collectDir "telemetry.json") -Raw -Encoding UTF8) | ConvertFrom-Json
        $lines = @()
        $lines += "==== owo-agent 诊断摘要（$((Get-Date).ToString('o'))）===="
        $lines += ""
        $lines += "-- SLO（/metrics/slo）--"
        if (@($sloObj.slo).Count -gt 0) {
            foreach ($s in @($sloObj.slo)) {
                $p95 = if ($null -ne $s.p95_ms) { "$($s.p95_ms) ms" } else { "-" }
                $lines += ("  {0,-16} {1,-8} samples={2} p95={3} violations={4}" -f $s.name, $(if ($s.achieving) { "达标" } else { "未达标" }), $s.samples, $p95, $s.violations)
            }
        } else { $lines += "  无 SLO 数据（探针未接线或服务未起）" }
        $lines += ""
        $lines += "-- 用量（/usage/summary）--"
        $lines += ("  记录 {0} 条，单价 {1} $/Mtok" -f $usageObj.count, $usageObj.price_per_mtok)
        foreach ($d in @($usageObj.dimensions)) {
            $over = if ($d.budget -and $d.budget.exceeded) { " 超限!" } else { "" }
            $budget = if ($d.budget) { "  预算 {0}/{1} USD" -f $d.budget.spent_usd, $d.budget.limit_usd } else { "" }
            $lines += ("  {0,-14} calls={1} cost={2} USD{3}{4}" -f $d.dimension, $d.calls, $d.cost_usd, $budget, $over)
        }
        if ($usageObj.hard_stop) { $lines += "  硬熔断：$($usageObj.hard_stop_reason)" }
        $lines += ""
        $lines += "-- 可选遥测（/metrics/telemetry/status）--"
        $lines += ("  开关：{0}" -f $(if ($teleObj.enabled) { "开" } else { "关（默认）" }))
        $lines | Set-Content -Path (Join-Path $collectDir "summary.txt") -Encoding UTF8
    } catch {
        Write-Host "摘要生成跳过（缺数据或解析失败）：$_" -ForegroundColor Yellow
    }

    # 4) 打包 zip（诊断包保留）
    $zipPath = Join-Path $outDir "owo-diagnose.zip"
    if (Get-Command Compress-Archive -ErrorAction SilentlyContinue) {
        Compress-Archive -Path (Join-Path $collectDir "*") -DestinationPath $zipPath -Force
    }
    Write-Host "诊断包已生成：$zipPath" -ForegroundColor Green
    Write-Host "（收集内容：environment/health/overview/runtime/slo/usage/recent_traces/logs_summary/slo_alerts/usage_report/slo_weekly/telemetry/summary，敏感字段已脱敏）"
    exit 0
} catch {
    Write-Host "诊断收集失败：$_" -ForegroundColor Red
    exit 1
}
