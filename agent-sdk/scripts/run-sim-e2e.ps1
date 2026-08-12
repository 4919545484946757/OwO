# 一键后台模拟验收（不干扰桌面）：启动 headless 模拟 QQ + 模拟浏览器站 + 测试服务，
# 依次跑 QQ 回复闭环与浏览器搜索/下载任务，结束后清理进程。
#
# 用法：
#   $env:OPENAI_API_KEY="sk-..."; $env:OPENAI_BASE_URL="https://api.deepseek.com/v1"
#   powershell -ExecutionPolicy Bypass -File scripts\run-sim-e2e.ps1
param(
    [int]$Port = 4097,
    [int]$SimQqPort = 18500,
    [int]$SimBrowserPort = 18201
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$root = Split-Path $PSScriptRoot -Parent
$debug = Join-Path $root "target\debug"
$python = "C:\Users\23843\.cache\codex-runtimes\codex-primary-runtime\dependencies\python\python.exe"

function Start-Hidden($exe, $arguments) {
    Start-Process -FilePath $exe -ArgumentList $arguments -WindowStyle Hidden -PassThru
}

$env:OWO_SIM_QQ_URL = "http://127.0.0.1:$SimQqPort"
$env:OWO_AUTO_APPROVE = "1"
$env:OWO_BROWSER_HEADLESS = "1"
if (-not $env:OWO_BROWSER_NODE) {
    $env:OWO_BROWSER_NODE = "C:\Users\23843\.cache\codex-runtimes\codex-primary-runtime\dependencies\node\bin\node.exe"
}
if (-not $env:OWO_BROWSER_NODE_PATH) {
    $env:OWO_BROWSER_NODE_PATH = "C:\Users\23843\.cache\codex-runtimes\codex-primary-runtime\dependencies\node\node_modules"
}

$simQq = Start-Hidden (Join-Path $debug "owo-sim-qq.exe") @("--headless", "--port", "$SimQqPort", "--log", (Join-Path $root "sim\logs\e2e.jsonl"))
$simBrowser = Start-Hidden (Join-Path $debug "owo-sim-browser.exe") @("--port", "$SimBrowserPort")
$server = Start-Hidden (Join-Path $debug "owo-agent.exe") @("serve", "--port", "$Port", "--workspace", $root)

try {
    $ready = $false
    for ($i = 0; $i -lt 30; $i++) {
        Start-Sleep -Milliseconds 500
        try {
            $null = Invoke-WebRequest -Uri "http://127.0.0.1:$Port/health" -TimeoutSec 2 -UseBasicParsing
            $ready = $true
            break
        } catch {}
    }
    if (-not $ready) { throw "测试服务未就绪（端口 $Port）" }

    Write-Host "== QQ 回复闭环 ==" -ForegroundColor Cyan
    & $python (Join-Path $PSScriptRoot "sim-qq-e2e.py") --base "http://127.0.0.1:$Port" --sim "http://127.0.0.1:$SimQqPort" --workspace $root
    if ($LASTEXITCODE -ne 0) { throw "QQ e2e 失败" }

    Write-Host "== 浏览器搜索/下载 ==" -ForegroundColor Cyan
    & $python (Join-Path $PSScriptRoot "sim-browser-e2e.py") --base "http://127.0.0.1:$Port" --browser "http://127.0.0.1:$SimBrowserPort" --workspace $root
    if ($LASTEXITCODE -ne 0) { throw "浏览器 e2e 失败" }

    Write-Host "全部模拟验收 PASS" -ForegroundColor Green
} finally {
    Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
    Stop-Process -Id $simBrowser.Id -Force -ErrorAction SilentlyContinue
    Stop-Process -Id $simQq.Id -Force -ErrorAction SilentlyContinue
}
