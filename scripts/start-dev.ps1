[CmdletBinding()]
param(
    [string]$BuildDirectory,
    [string]$LexiconPath,
    [string]$LibimeRuntimeDirectory,
    [string]$LibimeModelPath,
    [switch]$RestartCore,
    [switch]$DisableModelHost,
    [switch]$SkipRegistration,
    [switch]$OpenSettings,
    [switch]$OpenNotepad
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$projectRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$explicitBuildDirectory = -not [string]::IsNullOrWhiteSpace($BuildDirectory)
if ([string]::IsNullOrWhiteSpace($BuildDirectory)) {
    $BuildDirectory = Get-ChildItem -LiteralPath (Join-Path $projectRoot 'build') -Directory |
        ForEach-Object { Join-Path $_.FullName 'Release' } |
        Where-Object {
            (Test-Path -LiteralPath (Join-Path $_ 'owo_core_service.exe') -PathType Leaf) -and
            (Test-Path -LiteralPath (Join-Path $_ 'owo_tsf_profile_check.exe') -PathType Leaf) -and
            (Test-Path -LiteralPath (Join-Path $_ 'owo_ipc_shell.exe') -PathType Leaf)
        } |
        Sort-Object { (Get-Item -LiteralPath (Join-Path $_ 'owo_core_service.exe')).LastWriteTime } `
            -Descending |
        Select-Object -First 1
    if ([string]::IsNullOrWhiteSpace($BuildDirectory)) {
        throw 'No complete Release development build was found under build/.'
    }
}
$BuildDirectory = [IO.Path]::GetFullPath($BuildDirectory)

$coreService = Join-Path $BuildDirectory 'owo_core_service.exe'
$ipcShell = Join-Path $BuildDirectory 'owo_ipc_shell.exe'
$modelHost = Join-Path $BuildDirectory 'owo_model_host.exe'
$modelShell = Join-Path $BuildDirectory 'owo_model_shell.exe'
$profileCheck = Join-Path $BuildDirectory 'owo_tsf_profile_check.exe'
$buildTsfDll = Join-Path $BuildDirectory 'OwO.TSF.dll'
$deployedTsf = Get-ChildItem -LiteralPath (Join-Path $projectRoot 'build/manual-deploy') `
        -Filter 'OwO.TSF.*.dll' -File -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
$tsfDll = if ($explicitBuildDirectory -or $null -eq $deployedTsf) {
    $buildTsfDll
} else {
    $deployedTsf.FullName
}
$settingsCenter = Join-Path $projectRoot `
    'apps/settings_center/bin/x64/Release/net10.0-windows10.0.26100.0/win-x64/OwO.Settings.exe'

foreach ($required in @($coreService, $profileCheck, $tsfDll)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "Required development binary is missing: $required"
    }
}

if ([string]::IsNullOrWhiteSpace($LexiconPath)) {
    $LexiconPath = Join-Path $projectRoot `
        'build/windows-release/rime-ice-cn-2026.06.30-v2.owolx'
}
$LexiconPath = [IO.Path]::GetFullPath($LexiconPath)
if (-not (Test-Path -LiteralPath $LexiconPath -PathType Leaf)) {
    throw "Compiled lexicon is missing: $LexiconPath"
}

if ([string]::IsNullOrWhiteSpace($LibimeRuntimeDirectory)) {
    $LibimeRuntimeDirectory = Join-Path $projectRoot `
        'build/dependencies/libime-1.1.15/runtime-x86_64'
}
if ([string]::IsNullOrWhiteSpace($LibimeModelPath)) {
    $LibimeModelPath = Join-Path $projectRoot `
        'build/dependencies/libime-1.1.15/model-20260804/lib/libime/zh_CN.lm'
}
$LibimeRuntimeDirectory = [IO.Path]::GetFullPath($LibimeRuntimeDirectory)
$LibimeModelPath = [IO.Path]::GetFullPath($LibimeModelPath)
$libimeBridge = Join-Path $LibimeRuntimeDirectory 'owo_libime_bridge.dll'
$modelHostEnabled = -not $DisableModelHost -and
    (Test-Path -LiteralPath $modelHost -PathType Leaf) -and
    (Test-Path -LiteralPath $modelShell -PathType Leaf) -and
    (Test-Path -LiteralPath $libimeBridge -PathType Leaf) -and
    (Test-Path -LiteralPath $LibimeModelPath -PathType Leaf)

if (-not $SkipRegistration) {
    & (Join-Path $PSScriptRoot 'register-dev.ps1') -Configuration Release -DllPath $tsfDll
}

& $profileCheck --activate-session
if ($LASTEXITCODE -ne 0) { throw "OwO input profile could not be activated: $LASTEXITCODE" }

$running = @(Get-Process -Name 'owo_core_service' -ErrorAction SilentlyContinue)
$modelRunning = @(Get-Process -Name 'owo_model_host' -ErrorAction SilentlyContinue)
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
if ($RestartCore -and $modelRunning.Count -ne 0) {
    if (Test-Path -LiteralPath $modelShell -PathType Leaf) {
        & $modelShell --shutdown 2>$null | Out-Null
        Start-Sleep -Milliseconds 300
    }
    $modelRunning = @(Get-Process -Name 'owo_model_host' -ErrorAction SilentlyContinue)
    foreach ($process in $modelRunning) {
        Stop-Process -Id $process.Id -Force
    }
    $modelRunning = @()
}

$dataRoot = Join-Path $env:LOCALAPPDATA 'OwO/InputMethod'
$frequencyDirectory = Join-Path $dataRoot 'data'
$logDirectory = Join-Path $dataRoot 'logs'
$runDirectory = Join-Path $dataRoot 'run'
foreach ($directory in @($frequencyDirectory, $logDirectory, $runDirectory)) {
    [void](New-Item -ItemType Directory -Path $directory -Force)
}
$frequencyPath = Join-Path $frequencyDirectory 'user-frequency.owuf'

if ($modelHostEnabled -and $modelRunning.Count -eq 0) {
    $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $modelStdout = Join-Path $logDirectory "model-$stamp.stdout.log"
    $modelStderr = Join-Path $logDirectory "model-$stamp.stderr.log"
    $modelArguments = @(
        '--libime-bridge', ('"{0}"' -f $libimeBridge),
        '--libime-model', ('"{0}"' -f $LibimeModelPath)
    )
    $modelProcess = Start-Process -FilePath $modelHost -ArgumentList $modelArguments `
        -WorkingDirectory $projectRoot -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput $modelStdout -RedirectStandardError $modelStderr
    Start-Sleep -Milliseconds 750
    $modelProcess.Refresh()
    if ($modelProcess.HasExited) {
        $detail = if (Test-Path -LiteralPath $modelStderr) {
            (Get-Content -Raw -LiteralPath $modelStderr).Trim()
        } else { '' }
        throw "OwO ModelHost exited during startup (code $($modelProcess.ExitCode)): $detail"
    }
    Set-Content -LiteralPath (Join-Path $runDirectory 'model-host.pid') `
        -Value $modelProcess.Id -Encoding ASCII
    $modelRunning = @($modelProcess)
}

if ($running.Count -eq 0) {
    $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $stdout = Join-Path $logDirectory "core-$stamp.stdout.log"
    $stderr = Join-Path $logDirectory "core-$stamp.stderr.log"
    $arguments = @(
        '--lexicon', ('"{0}"' -f $LexiconPath),
        '--user-frequency', ('"{0}"' -f $frequencyPath)
    )
    if ($modelHostEnabled) { $arguments += '--model-host' }
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
if ($modelHostEnabled) {
    Write-Output "ModelHost PID: $(($modelRunning | ForEach-Object { $_.Id }) -join ', ')"
    Write-Output "libime model: $LibimeModelPath"
} else {
    Write-Output 'ModelHost: disabled or libime runtime/model is incomplete; using base ranking.'
}
Write-Output "TSF DLL: $tsfDll"
Write-Output "Lexicon: $LexiconPath"
Write-Output "User frequency: $frequencyPath"
Write-Output 'OwO is activated for this Windows session; use Win+Space if the target app kept another profile.'
