# OwO Agent 便携打包：核心服务 + 桌面壳 + 内置技能包 → dist/OwO-Agent-<配置>.zip
# 用法：powershell -ExecutionPolicy Bypass -File scripts\package-desktop.ps1 [-Configuration release|debug]
param(
    [ValidateSet("release", "debug")]
    [string]$Configuration = "release"
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$cargo = if ($env:OWO_CARGO) {
    $env:OWO_CARGO
} elseif (Test-Path (Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe")) {
    Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
} else {
    "cargo"
}

$root = Split-Path $PSScriptRoot -Parent
$dist = Join-Path $root "dist\OwO-Agent"
if (Test-Path $dist) {
    Remove-Item -LiteralPath $dist -Recurse -Force
}
New-Item -ItemType Directory -Path $dist -Force | Out-Null

Push-Location $root
try {
    Write-Host "[package] 构建核心服务（$Configuration）..."
    & $cargo build -p owo-agent-cli "--$Configuration"
    if ($LASTEXITCODE -ne 0) { throw "核心服务构建失败" }
} finally {
    Pop-Location
}

Push-Location (Join-Path $root "desktop\tauri\src-tauri")
try {
    Write-Host "[package] 构建桌面壳（$Configuration）..."
    & $cargo build "--$Configuration"
    if ($LASTEXITCODE -ne 0) { throw "桌面壳构建失败" }
} finally {
    Pop-Location
}

$targetDir = Join-Path $root "target\$Configuration"
$desktopTarget = Join-Path $root "desktop\tauri\src-tauri\target\$Configuration"
Copy-Item -LiteralPath (Join-Path $targetDir "owo-agent.exe") -Destination $dist
Copy-Item -LiteralPath (Join-Path $desktopTarget "owo-agent-desktop.exe") -Destination $dist
Copy-Item -LiteralPath (Join-Path $root "skills") -Destination $dist -Recurse
Copy-Item -LiteralPath (Join-Path $root "settings.example.json") -Destination (Join-Path $dist "settings.example.json")

@"
OwO Agent 便携版（v0.4 P1/P2/P3）

运行：双击 owo-agent-desktop.exe（自动拉起同目录 owo-agent.exe 核心服务，端口 4096）。
快捷键：Ctrl+Alt+Shift+O 唤起工作台。

环境变量（可选）：
  OPENAI_API_KEY / OPENAI_BASE_URL / OPENAI_MODEL  模型凭据（默认 DeepSeek 兼容端点需自行设置）
  OWO_AGENT_DATA                                    数据目录（会话/审计/技能，默认 %LOCALAPPDATA%\OwO\Agent）
  OWO_SKILLS_DIR                                    内置技能包目录（默认使用随包 skills/）

安全：权限默认 deny；写/执行/注入需审批；密码/支付/验证码类锚点熔断不执行。
"@ | Set-Content -LiteralPath (Join-Path $dist "README.txt") -Encoding UTF8

$zip = Join-Path $root "dist\OwO-Agent-$Configuration.zip"
if (Test-Path $zip) {
    Remove-Item -LiteralPath $zip -Force
}
Compress-Archive -Path (Join-Path $dist "*") -DestinationPath $zip
Write-Host "[package] 完成：$zip"
