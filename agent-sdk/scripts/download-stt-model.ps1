# 下载 SenseVoice-Small（int8，约 240MB）到本地 STT 模型目录。
# 用法：powershell -ExecutionPolicy Bypass -File scripts\download-stt-model.ps1
param()

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$dataRoot = if ($env:OWO_AGENT_DATA) {
    $env:OWO_AGENT_DATA
} else {
    Join-Path $env:LOCALAPPDATA "OwO\Agent"
}
$targetDir = Join-Path $dataRoot "models\stt\SenseVoice-Small"
New-Item -ItemType Directory -Force -Path $targetDir | Out-Null

if ((Test-Path (Join-Path $targetDir "model.int8.onnx")) -and (Test-Path (Join-Path $targetDir "tokens.txt"))) {
    Write-Host "[stt] 模型已就绪：$targetDir"
    exit 0
}

$url = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17-int8.tar.bz2"
$tmp = Join-Path $env:TEMP "sherpa-onnx-sense-voice-int8.tar.bz2"
$extract = Join-Path $env:TEMP "sherpa-onnx-sense-voice-int8"

Write-Host "[stt] 下载 SenseVoice-Small（约 240MB，可设置 HTTPS_PROXY 加速）..."
curl.exe -L -o $tmp $url
if ($LASTEXITCODE -ne 0) { throw "下载失败" }

if (Test-Path $extract) { Remove-Item -LiteralPath $extract -Recurse -Force }
New-Item -ItemType Directory -Path $extract | Out-Null
tar -xjf $tmp -C $extract
if ($LASTEXITCODE -ne 0) { throw "解压失败" }

$modelRoot = Get-ChildItem $extract -Directory | Select-Object -First 1
Copy-Item -LiteralPath (Join-Path $modelRoot.FullName "model.int8.onnx") -Destination $targetDir
Copy-Item -LiteralPath (Join-Path $modelRoot.FullName "tokens.txt") -Destination $targetDir
Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
Remove-Item -LiteralPath $extract -Recurse -Force

Write-Host "[stt] 完成：$targetDir"
