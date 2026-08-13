# 在用户交互会话启动核心服务（真实桌面输入必需）。
# 用法：
#   $env:OPENAI_API_KEY="sk-..."
#   powershell -ExecutionPolicy Bypass -File agent-sdk\scripts\start-dev-service.ps1
# 端口/工作区可覆盖：-Port 4096 -Workspace D:\...
param(
    [int]$Port = 4096,
    [string]$Workspace = (Split-Path $PSScriptRoot -Parent),
    [switch]$AutoApprove
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

if (-not $env:OPENAI_API_KEY) {
    Write-Host "请先设置 OPENAI_API_KEY（例如：`$env:OPENAI_API_KEY=`"sk-...`"）" -ForegroundColor Red
    exit 1
}

$env:OPENAI_BASE_URL = if ($env:OPENAI_BASE_URL) { $env:OPENAI_BASE_URL } else { "https://api.deepseek.com/v1" }
$env:OPENAI_MODEL = if ($env:OPENAI_MODEL) { $env:OPENAI_MODEL } else { "deepseek-v4-flash" }
if (-not $env:OWO_HTTP_PROXY) { $env:OWO_HTTP_PROXY = "http://127.0.0.1:7897" }
if (-not $env:PADDLE_OCR_MODEL) { $env:PADDLE_OCR_MODEL = "PP-OCRv6" }
if (-not $env:OWO_CLOUD_ENABLED) { $env:OWO_CLOUD_ENABLED = "true" }
if (-not $env:OWO_VISION_PROVIDER) { $env:OWO_VISION_PROVIDER = "ollama" }
if (-not $env:OWO_VISION_MODEL) { $env:OWO_VISION_MODEL = "qwen2.5vl:3b" }
if (-not $env:OWO_BROWSER_NODE) {
    $env:OWO_BROWSER_NODE = "C:\Users\23843\.cache\codex-runtimes\codex-primary-runtime\dependencies\node\bin\node.exe"
}
if (-not $env:OWO_BROWSER_NODE_PATH) {
    $env:OWO_BROWSER_NODE_PATH = "C:\Users\23843\.cache\codex-runtimes\codex-primary-runtime\dependencies\node\node_modules"
}
if ($AutoApprove) { $env:OWO_AUTO_APPROVE = "1" }

$exe = Join-Path (Split-Path $PSScriptRoot -Parent) "target\debug\owo-agent.exe"
if (-not (Test-Path $exe)) {
    Write-Host "未找到 $exe，请先构建：cargo build --workspace" -ForegroundColor Red
    exit 1
}

Write-Host "启动核心服务：$exe serve --port $Port --workspace $Workspace" -ForegroundColor Cyan
Write-Host "模型：$env:OPENAI_BASE_URL / $env:OPENAI_MODEL  端口：$Port" -ForegroundColor Cyan
Write-Host "自动审批：$($AutoApprove -or $env:OWO_AUTO_APPROVE)" -ForegroundColor Cyan
& $exe serve --port $Port --workspace $Workspace
