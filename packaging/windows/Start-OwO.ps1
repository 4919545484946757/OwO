[CmdletBinding()]
param([switch]$OpenSettings)

$ErrorActionPreference = 'Stop'
$installRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$launcher = Join-Path $installRoot 'bin\owo_runtime_launcher.exe'
if (-not (Test-Path -LiteralPath $launcher -PathType Leaf)) {
    throw "OwO native runtime launcher is missing: $launcher"
}
$arguments = if ($OpenSettings) { @('--open-settings') } else { @() }
& $launcher @arguments
if ($LASTEXITCODE -ne 0) {
    throw "OwO native runtime launcher failed: $LASTEXITCODE"
}
