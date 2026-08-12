# 构建 NSIS 安装程序（含核心服务 sidecar）。
# 用法：powershell -ExecutionPolicy Bypass -File scripts\build-installer.ps1
param()

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$root = Split-Path $PSScriptRoot -Parent
$tauriDir = Join-Path $root "desktop\tauri\src-tauri"
$cargo = if ($env:OWO_CARGO) { $env:OWO_CARGO } else { Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe" }
$npx = if ($env:OWO_NPX) { $env:OWO_NPX } else { "D:\前端框架\npx.cmd" }

Push-Location $root
try {
    Write-Host "[installer] 构建核心服务 release..."
    & $cargo build -p owo-agent-cli --release
    if ($LASTEXITCODE -ne 0) { throw "核心服务构建失败" }
} finally {
    Pop-Location
}

$binDir = Join-Path $tauriDir "binaries"
New-Item -ItemType Directory -Force -Path $binDir | Out-Null
Copy-Item -LiteralPath (Join-Path $root "target\release\owo-agent.exe") `
    -Destination (Join-Path $binDir "owo-agent.exe-x86_64-pc-windows-msvc.exe") -Force

Push-Location $tauriDir
try {
    Write-Host "[installer] 打包 NSIS（npx @tauri-apps/cli build）..."
    & $npx --yes @tauri-apps/cli@2 build
    if ($LASTEXITCODE -ne 0) { throw "NSIS 打包失败" }
} finally {
    Pop-Location
}

Write-Host "[installer] 完成：desktop\tauri\src-tauri\target\release\bundle\nsis\"
