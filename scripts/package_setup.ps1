[CmdletBinding()]
param(
    [string]$Version = '0.1.0-alpha.2',
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '..\artifacts\release'),
    [string]$CompilerPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$output = [IO.Path]::GetFullPath($OutputDirectory)
$releaseRoot = Join-Path $output $Version
$stage = Join-Path $releaseRoot "OwO-Input-Method-$Version-windows-x64"
$definition = Join-Path $root 'packaging\windows\OwO-Setup.iss'

if ($Version -notmatch '^(\d+)\.(\d+)\.(\d+)-alpha\.(\d+)$') {
    throw 'Setup version must use the form major.minor.patch-alpha.number.'
}
$numericVersion = "$($Matches[1]).$($Matches[2]).$($Matches[3]).$($Matches[4])"

if (-not (Test-Path -LiteralPath $stage -PathType Container)) {
    throw "Release staging directory is missing: $stage"
}
if (-not (Test-Path -LiteralPath $definition -PathType Leaf)) {
    throw "Inno Setup definition is missing: $definition"
}

if ([string]::IsNullOrWhiteSpace($CompilerPath)) {
    $candidates = @(
        (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe'),
        (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 7\ISCC.exe'),
        (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
        (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe'),
        (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 7\ISCC.exe'),
        (Join-Path $env:ProgramFiles 'Inno Setup 7\ISCC.exe')
    )
    $CompilerPath = $candidates |
        Where-Object { -not [string]::IsNullOrWhiteSpace($_) -and
                       (Test-Path -LiteralPath $_ -PathType Leaf) } |
        Select-Object -First 1
}
if ([string]::IsNullOrWhiteSpace($CompilerPath) -or
    -not (Test-Path -LiteralPath $CompilerPath -PathType Leaf)) {
    throw 'Inno Setup compiler was not found. Install JRSoftware.InnoSetup or pass -CompilerPath.'
}

& $CompilerPath "/DMyVersion=$Version" "/DNumericVersion=$numericVersion" `
    "/DStageDir=$stage" "/DReleaseDir=$releaseRoot" $definition
if ($LASTEXITCODE -ne 0) { throw "Inno Setup compilation failed: $LASTEXITCODE" }

$setup = Join-Path $releaseRoot "OwO-Input-Method-$Version-windows-x64-Setup.exe"
if (-not (Test-Path -LiteralPath $setup -PathType Leaf)) {
    throw "Setup output is missing: $setup"
}

$releaseFiles = @(
    (Join-Path $releaseRoot "OwO-Input-Method-$Version-windows-x64.zip"),
    (Join-Path $releaseRoot "OwO-Input-Method-$Version-source.zip"),
    $setup
) | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf }
$hashLines = foreach ($file in $releaseFiles) {
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $file).Hash.ToLowerInvariant()
    "$hash  $([IO.Path]::GetFileName($file))"
}
[IO.File]::WriteAllLines((Join-Path $releaseRoot 'SHA256SUMS.txt'), $hashLines,
                         [Text.UTF8Encoding]::new($false))
Write-Output $setup
