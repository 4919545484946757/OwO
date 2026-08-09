[CmdletBinding()]
param(
    [string]$BuildDirectory,
    [string]$LexiconPath,
    [switch]$RestartCore,
    [switch]$SkipRegistration,
    [switch]$OpenSettings,
    [switch]$OpenNotepad
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$projectRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
if ([string]::IsNullOrWhiteSpace($BuildDirectory)) {
    $BuildDirectory = Join-Path $projectRoot 'build/windows-manual-test/Release'
}
$BuildDirectory = [IO.Path]::GetFullPath($BuildDirectory)

$coreService = Join-Path $BuildDirectory 'owo_core_service.exe'
$ipcShell = Join-Path $BuildDirectory 'owo_ipc_shell.exe'
$profileCheck = Join-Path $BuildDirectory 'owo_tsf_profile_check.exe'
$tsfDll = Join-Path $BuildDirectory 'OwO.TSF.dll'
$settingsCenter = Join-Path $projectRoot `
    'apps/settings_center/bin/Release/net10.0-windows10.0.26100.0/win-x64/OwO.Settings.exe'

foreach ($required in @($coreService, $profileCheck, $tsfDll)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "Required development binary is missing: $required"
    }
}

if ([string]::IsNullOrWhiteSpace($LexiconPath)) {
    $LexiconPath = Join-Path $projectRoot `
        'build/windows-release/rime-ice-cn-2026.06.30.owolx'
}
$LexiconPath = [IO.Path]::GetFullPath($LexiconPath)
if (-not (Test-Path -LiteralPath $LexiconPath -PathType Leaf)) {
    throw "Compiled lexicon is missing: $LexiconPath"
}

if (-not $SkipRegistration) {
    & (Join-Path $PSScriptRoot 'register-dev.ps1') -Configuration Release -DllPath $tsfDll
}

& $profileCheck --enable
if ($LASTEXITCODE -ne 0) { throw "OwO input profile could not be enabled: $LASTEXITCODE" }

$running = @(Get-Process -Name 'owo_core_service' -ErrorAction SilentlyContinue)
if ($RestartCore -and $running.Count -ne 0) {
    if (Test-Path -LiteralPath $ipcShell -PathType Leaf) {
        & $ipcShell --shutdown 2>$null | Out-Null
        Start-Sleep -Milliseconds 300
    }
    $running = @(Get-Process -Name 'owo_core_service' -ErrorAction SilentlyContinue)
    foreach ($process in $running) {
        Stop-Process -Id $process.Id -Force
    }
    $running = @()
}

$dataRoot = Join-Path $env:LOCALAPPDATA 'OwO/InputMethod'
$frequencyDirectory = Join-Path $dataRoot 'data'
$logDirectory = Join-Path $dataRoot 'logs'
$runDirectory = Join-Path $dataRoot 'run'
foreach ($directory in @($frequencyDirectory, $logDirectory, $runDirectory)) {
    [void](New-Item -ItemType Directory -Path $directory -Force)
}
$frequencyPath = Join-Path $frequencyDirectory 'user-frequency.owuf'

if ($running.Count -eq 0) {
    $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $stdout = Join-Path $logDirectory "core-$stamp.stdout.log"
    $stderr = Join-Path $logDirectory "core-$stamp.stderr.log"
    $arguments = @(
        '--lexicon', ('"{0}"' -f $LexiconPath),
        '--user-frequency', ('"{0}"' -f $frequencyPath)
    )
    $core = Start-Process -FilePath $coreService -ArgumentList $arguments `
        -WorkingDirectory $projectRoot -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    Start-Sleep -Milliseconds 750
    $core.Refresh()
    if ($core.HasExited) {
        $detail = if (Test-Path -LiteralPath $stderr) {
            (Get-Content -Raw -LiteralPath $stderr).Trim()
        } else { '' }
        throw "OwO Core Service exited during startup (code $($core.ExitCode)): $detail"
    }
    Set-Content -LiteralPath (Join-Path $runDirectory 'core-service.pid') `
        -Value $core.Id -Encoding ASCII
    $running = @($core)
}

if ($OpenSettings) {
    if (-not (Test-Path -LiteralPath $settingsCenter -PathType Leaf)) {
        throw "Settings center is missing: $settingsCenter"
    }
    Start-Process -FilePath $settingsCenter
}
if ($OpenNotepad) {
    Start-Process -FilePath "$env:SystemRoot/System32/notepad.exe"
}

$processIds = ($running | ForEach-Object { $_.Id }) -join ', '
Write-Output "OwO manual-test environment is ready."
Write-Output "Core PID: $processIds"
Write-Output "TSF DLL: $tsfDll"
Write-Output "Lexicon: $LexiconPath"
Write-Output "User frequency: $frequencyPath"
Write-Output 'Use Win+Space in the target app and select OwO.'
