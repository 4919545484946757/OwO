[CmdletBinding()]
param([switch]$ForceRebuild)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$projectRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$publishDirectory = Join-Path $projectRoot 'build/settings-publish/win-x64'
$settingsCenter = Join-Path $publishDirectory 'OwO.Settings.exe'

$sourceRoots = @(
    (Join-Path $projectRoot 'apps/settings_center'),
    (Join-Path $projectRoot 'apps/config_shell'),
    (Join-Path $projectRoot 'include/owo/config'),
    (Join-Path $projectRoot 'src/config')
)
$latestSource = $sourceRoots | Where-Object { Test-Path -LiteralPath $_ } |
    ForEach-Object { Get-ChildItem -LiteralPath $_ -File -Recurse } |
    Where-Object { $_.FullName -notmatch '[\\/](bin|obj)[\\/]' } |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
$needsRebuild = $ForceRebuild -or
    -not (Test-Path -LiteralPath $settingsCenter -PathType Leaf) -or
    ($null -ne $latestSource -and
     $latestSource.LastWriteTime -gt (Get-Item -LiteralPath $settingsCenter).LastWriteTime)

if ($needsRebuild) {
    Get-Process -Name 'OwO.Settings' -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -eq $settingsCenter } | Stop-Process -Force
    & (Join-Path $PSScriptRoot 'build_settings_center.ps1') `
        -Configuration Release -RuntimeIdentifier win-x64 -NoRestore
}

if (-not (Test-Path -LiteralPath $settingsCenter -PathType Leaf)) {
    throw "Settings center is missing after build: $settingsCenter"
}
Start-Process -FilePath $settingsCenter -WorkingDirectory $publishDirectory
Write-Output "OwO settings center opened: $settingsCenter"
