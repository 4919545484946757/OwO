# 生成 Tauri updater 动态清单 latest.json（稳定/测试通道 + cohort 灰度 + 失败率自动暂停）。
# 用法：powershell -ExecutionPolicy Bypass -File scripts\generate-update-manifest.ps1 `
#         -SetupExe dist\OwO-Agent-0.1.0-setup.exe -Version 0.1.0 -BaseUrl https://example.com/owo/updates `
#         [-Channel stable|beta] [-Cohort 0.1] [-RolloutFailureThreshold 0.05] [-PreviousManifest dist\updates\latest.json]
param(
    [string]$SetupExe = "",
    [string]$Version = "0.1.0",
    [string]$Notes = "OwO Agent 自动更新",
    [string]$BaseUrl = "https://example.com/owo/updates",
    # R10：发布通道（stable 全量；beta 预览）。
    [ValidateSet("stable", "beta")]
    [string]$Channel = "stable",
    # R10：灰度 cohort（如 "0.1" = 首批 10% 用户；空 = 不限制）。
    [string]$Cohort = "",
    # R10：失败率阈值（0..1），上一次清单同版本失败率 ≥ 阈值时自动暂停（paused=true）。
    [double]$RolloutFailureThreshold = 0.05,
    # R10：上一次发布的清单（用于失败率判断；缺省不暂停）。
    [string]$PreviousManifest = ""
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$root = Split-Path $PSScriptRoot -Parent
$tauriDir = Join-Path $root "desktop\tauri\src-tauri"
$key = Join-Path $tauriDir ".secrets\owo-update.key"
$passFile = Join-Path $tauriDir ".secrets\owo-update.pass"
$npx = if ($env:OWO_NPX) { $env:OWO_NPX } else { "D:\前端框架\npx.cmd" }

if (-not (Test-Path $key)) {
    throw "缺少签名私钥：$key（先运行 npx @tauri-apps/cli signer generate -w $key --ci）"
}
if (-not $SetupExe) {
    $SetupExe = (Get-ChildItem (Join-Path $tauriDir "target\release\bundle\nsis\*-setup.exe") | Select-Object -First 1).FullName
}
if (-not (Test-Path $SetupExe)) {
    throw "安装包不存在：$SetupExe"
}

$env:PATH = "C:\Users\23843\.cargo\bin;" + $env:PATH
$env:TAURI_SIGNING_PRIVATE_KEY_PATH = $key
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = (Get-Content $passFile -Raw).Trim()

$output = & $npx --yes @tauri-apps/cli@2 signer sign $SetupExe 2>&1 | Out-String
if ($LASTEXITCODE -ne 0) {
    throw "签名失败：$output"
}
$lines = $output -split "`r?`n"
$signature = ""
for ($i = 0; $i -lt $lines.Length; $i++) {
    if ($lines[$i].Trim() -eq "Public signature:") {
        $signature = $lines[$i + 1].Trim()
        break
    }
}
if (-not $signature) {
    throw "无法从签名输出中提取 signature"
}

$fileName = Split-Path $SetupExe -Leaf

# R10：失败率自动暂停——上一次清单同版本失败率 ≥ 阈值 → paused=true（停止放量）。
$paused = $false
$pausedReason = ""
$failureRate = $null
if ($PreviousManifest -and (Test-Path $PreviousManifest)) {
    try {
        $previous = Get-Content -LiteralPath $PreviousManifest -Raw -Encoding UTF8 | ConvertFrom-Json
        if ($previous.version -eq $Version -and $null -ne $previous.failureRate) {
            $failureRate = [double]$previous.failureRate
            if ($failureRate -ge $RolloutFailureThreshold) {
                $paused = $true
                $pausedReason = "上一清单失败率 $($failureRate.ToString("P1")) ≥ 阈值 $($RolloutFailureThreshold.ToString("P1"))"
            }
        }
    } catch {
        Write-Host "[updater] 上一清单解析失败（$($_.Exception.Message)），按未暂停处理"
    }
}

$manifest = @{
    version   = $Version
    notes     = $Notes
    pub_date  = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    # R10：发布治理字段。
    channel   = $Channel
    cohort    = $Cohort
    rolloutFailureThreshold = $RolloutFailureThreshold
    paused    = $paused
    pausedReason = $pausedReason
    platforms = @{
        "windows-x86_64" = @{
            signature = $signature
            url       = "$BaseUrl/$fileName"
        }
    }
} | ConvertTo-Json -Depth 6

$outDir = Join-Path $root "dist\updates"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$outFile = Join-Path $outDir "latest.json"
Set-Content -LiteralPath $outFile -Value $manifest -Encoding UTF8
Write-Host "[updater] 清单已生成：$outFile"
Write-Host "[updater] channel=$Channel cohort=$(if ($Cohort) { $Cohort } else { 'all' }) paused=$paused"
Write-Host "[updater] signature 长度：$($signature.Length)"
