# 生成 Tauri updater 静态清单 latest.json（配合任意静态托管 / GitHub Pages）。
# 用法：powershell -ExecutionPolicy Bypass -File scripts\generate-update-manifest.ps1 `
#         -SetupExe dist\OwO-Agent-0.1.0-setup.exe -Version 0.1.0 -BaseUrl https://example.com/owo/updates
param(
    [string]$SetupExe = "",
    [string]$Version = "0.1.0",
    [string]$Notes = "OwO Agent 自动更新",
    [string]$BaseUrl = "https://example.com/owo/updates"
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
$manifest = @{
    version   = $Version
    notes     = $Notes
    pub_date  = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
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
Write-Host "[updater] signature 长度：$($signature.Length)"
