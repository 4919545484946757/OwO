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
$configArgs = @()
if ($Configuration -eq "release") {
    $configArgs = @("--release")
}
if (Test-Path $dist) {
    Remove-Item -LiteralPath $dist -Recurse -Force
}
New-Item -ItemType Directory -Path $dist -Force | Out-Null

Push-Location $root
try {
    Write-Host "[package] 构建核心服务（$Configuration）..."
    & $cargo build -p owo-agent-cli @configArgs
    if ($LASTEXITCODE -ne 0) { throw "核心服务构建失败" }
} finally {
    Pop-Location
}

Push-Location (Join-Path $root "desktop\tauri\src-tauri")
try {
    Write-Host "[package] 构建桌面壳（$Configuration）..."
    & $cargo build @configArgs
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
if (Test-Path (Join-Path $root "models\ocr")) {
    Write-Host "[package] 附带本地 ONNX OCR 模型（models/ocr，离线确定性 OCR 通道）..."
    Copy-Item -LiteralPath (Join-Path $root "models") -Destination $dist -Recurse
}

# ONNX Runtime 动态库：ort 按 load-dynamic 加载 onnxruntime.dll（exe 同级优先）。
$onnxRuntimeDll = Join-Path $dist "onnxruntime.dll"
if (-not (Test-Path $onnxRuntimeDll)) {
    $builtDll = Join-Path $targetDir "onnxruntime.dll"
    if (Test-Path $builtDll) {
        Copy-Item -LiteralPath $builtDll -Destination $onnxRuntimeDll
    } else {
        Write-Host "[package] 下载 ONNX Runtime 1.28.0 x64（onnxruntime.dll，约 22MB）..."
        $ortZip = Join-Path $env:TEMP "onnxruntime-win-x64-1.28.0.zip"
        Invoke-WebRequest -Uri "https://github.com/microsoft/onnxruntime/releases/download/v1.28.0/onnxruntime-win-x64-1.28.0.zip" -OutFile $ortZip -UseBasicParsing
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $ortArchive = [System.IO.Compression.ZipFile]::OpenRead($ortZip)
        try {
            $entry = $ortArchive.GetEntry("onnxruntime-win-x64-1.28.0/lib/onnxruntime.dll")
            if ($null -eq $entry) { throw "onnxruntime 包内缺少 lib\onnxruntime.dll" }
            [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $onnxRuntimeDll, $true)
        } finally {
            $ortArchive.Dispose()
        }
        Remove-Item $ortZip -Force
    }
    Write-Host "[package] onnxruntime.dll 已就位（$onnxRuntimeDll）"
}

@"
OwO Agent 便携版（v0.4 P1/P2/P3 + v0.5 M-E）

运行：双击 owo-agent-desktop.exe（自动拉起同目录 owo-agent.exe 核心服务，端口 4096）。
快捷键：Ctrl+Alt+Shift+O 唤起工作台。

环境变量（可选）：
  OPENAI_API_KEY / OPENAI_BASE_URL / OPENAI_MODEL  模型凭据（默认 DeepSeek 兼容端点需自行设置）
  OWO_AGENT_DATA                                    数据目录（会话/审计/技能，默认 %LOCALAPPDATA%\OwO\Agent）
  OWO_SKILLS_DIR                                    内置技能包目录（默认使用随包 skills/）
  OWO_ONNX_OCR_MODEL_DIR                            本地 ONNX OCR 模型目录（默认 models/ocr 或数据目录）

OCR 通道优先级：本地 ONNX（随包/数据目录，无网可用）→ Paddle 云（需 PADDLE_OCR_TOKEN）→ Windows Media.Ocr。

安全：权限默认 deny；写/执行/注入需审批；密码/支付/验证码类锚点熔断不执行。
"@ | Set-Content -LiteralPath (Join-Path $dist "README.txt") -Encoding UTF8

$zip = Join-Path $root "dist\OwO-Agent-$Configuration.zip"
if (Test-Path $zip) {
    Remove-Item -LiteralPath $zip -Force
}
Compress-Archive -Path (Join-Path $dist "*") -DestinationPath $zip
Write-Host "[package] 完成：$zip"
