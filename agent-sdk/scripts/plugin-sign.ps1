# plugin-sign.ps1 — 插件签名包装器（Ed25519，调用 plugin-sign.py）
#
# 用法（示例插件）：
#   .\scripts\plugin-sign.ps1 generate -KeyFile "$env:TEMP\owo-plugin-key.pem"
#   .\scripts\plugin-sign.ps1 sign -PluginDir .\plugins\owo-translate -KeyFile "$env:TEMP\owo-plugin-key.pem"
#   .\scripts\plugin-sign.ps1 verify -PluginDir .\plugins\owo-translate -KeyFile "$env:TEMP\owo-plugin-key.pem"
#   .\scripts\plugin-sign.ps1 verify -PluginDir .\plugins\owo-translate   # 用 manifest 内公钥验签
#
# 摘要口径与 core::plugin::plugin_digest 一致（Rust 端校验）。
# 私钥不入库：生成到 $env:TEMP 或用户私有目录。

param(
    [Parameter(Mandatory = $true)][ValidateSet("generate", "sign", "verify")][string]$Action,
    [string]$PluginDir,
    [string]$KeyFile,
    [string]$PublicKeyB64
)

$ErrorActionPreference = "Stop"
$script = Join-Path $PSScriptRoot "plugin-sign.py"
if (-not (Test-Path $script)) { Write-Error "缺少 $script"; exit 2 }

$argsList = @($Action)
if ($PluginDir) { $argsList += @("--plugin-dir", $PluginDir) }
if ($KeyFile) { $argsList += @("--key-file", $KeyFile) }
if ($PublicKeyB64) { $argsList += @("--public-key-b64", $PublicKeyB64) }

& python $script @argsList
exit $LASTEXITCODE
