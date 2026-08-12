# 下载本地视觉模型（Ollama），默认 qwen2.5vl:3b（约 3.2GB）。
# 用法：powershell -ExecutionPolicy Bypass -File scripts\download-vision-model.ps1 [-Model qwen2.5vl:3b]
param(
    [string]$Model = "qwen2.5vl:3b"
)

$ollama = Join-Path $env:LOCALAPPDATA "Programs\Ollama\ollama.exe"
if (-not (Test-Path $ollama)) {
    Write-Host "未找到 Ollama：$ollama（请先安装 https://ollama.com）" -ForegroundColor Red
    exit 1
}
Write-Host "正在拉取 $Model ..." -ForegroundColor Cyan
& $ollama pull $Model
Write-Host "完成。检查：ollama list" -ForegroundColor Green
