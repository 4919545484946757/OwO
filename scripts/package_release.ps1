[CmdletBinding()]
param(
    [string]$Version = '0.1.0-alpha.1',
    [string]$BuildDirectory = (Join-Path $PSScriptRoot '..\build\windows-release\Release'),
    [string]$SettingsDirectory = (Join-Path $PSScriptRoot '..\build\settings-publish\win-x64'),
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '..\artifacts\release')
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$build = [IO.Path]::GetFullPath($BuildDirectory)
$output = [IO.Path]::GetFullPath($OutputDirectory)
$releaseRoot = Join-Path $output $Version
$stage = Join-Path $releaseRoot "OwO-Input-Method-$Version-windows-x64"
if (-not $releaseRoot.StartsWith($output + [IO.Path]::DirectorySeparatorChar,
                                 [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Invalid release staging path.'
}
if (Test-Path -LiteralPath $releaseRoot) {
    Remove-Item -LiteralPath $releaseRoot -Recurse -Force
}
[void](New-Item -ItemType Directory -Path $stage -Force)

$settings = [IO.Path]::GetFullPath($SettingsDirectory)
$runtime = Join-Path $root 'build\dependencies\libime-1.1.15\runtime-p1-x86_64'
$model = Join-Path $root 'build\dependencies\libime-1.1.15\model-20260804\lib\libime\zh_CN.lm'
$lexicon = Join-Path $root 'build\windows-release\rime-ice-cn-2026.06.30-v2.owolx'
$vcBuildTools = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools'
$vcRedistBase = Join-Path $vcBuildTools 'VC\Redist\MSVC'
$vcRuntime = Get-ChildItem -LiteralPath $vcRedistBase -Directory -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -match '^\d+(\.\d+)+$' } |
    Sort-Object { [Version]$_.Name } -Descending |
    ForEach-Object { Join-Path $_.FullName 'x64\Microsoft.VC143.CRT' } |
    Where-Object { Test-Path -LiteralPath $_ -PathType Container } |
    Select-Object -First 1
$required = @(
    (Join-Path $build 'OwO.TSF.dll'), (Join-Path $build 'owo_core_service.exe'),
    (Join-Path $build 'owo_model_host.exe'), (Join-Path $build 'owo_runtime_launcher.exe'),
    (Join-Path $build 'owo_tsf_profile_check.exe'),
    (Join-Path $settings 'OwO.Settings.exe'),
    (Join-Path $settings 'OwO.Settings.dll'),
    (Join-Path $settings 'App.xbf'),
    (Join-Path $settings 'MainPage.xbf'),
    (Join-Path $settings 'MainWindow.xbf'),
    (Join-Path $settings 'OwO.Settings.pri'),
    (Join-Path $settings 'Assets\AppIcon.ico'),
    (Join-Path $settings 'owo_config_shell.exe'),
    (Join-Path $settings 'owo_plugin_shell.exe'),
    $runtime, $model, $lexicon, $vcRuntime
)
foreach ($path in $required) {
    if (-not (Test-Path -LiteralPath $path)) { throw "Release input is missing: $path" }
}

foreach ($directory in @('bin', 'data', 'model\runtime', 'settings', 'scripts', 'LICENSES')) {
    [void](New-Item -ItemType Directory -Path (Join-Path $stage $directory) -Force)
}
foreach ($name in @('OwO.TSF.dll', 'owo_core_service.exe', 'owo_model_host.exe',
                    'owo_runtime_launcher.exe', 'owo_tsf_profile_check.exe',
                    'owo_config_shell.exe', 'owo_plugin_shell.exe')) {
    Copy-Item -LiteralPath (Join-Path $build $name) -Destination (Join-Path $stage 'bin') -Force
}
Copy-Item -Path (Join-Path $vcRuntime '*.dll') -Destination (Join-Path $stage 'bin') -Force
Copy-Item -Path (Join-Path $settings '*') -Destination (Join-Path $stage 'settings') `
    -Recurse -Force
Copy-Item -Path (Join-Path $runtime '*') -Destination (Join-Path $stage 'model\runtime') `
    -Recurse -Force
Copy-Item -LiteralPath $model -Destination (Join-Path $stage 'model\zh_CN.lm') -Force
Copy-Item -LiteralPath $lexicon -Destination (Join-Path $stage 'data\owo-cn.owolx') -Force

$packaging = Join-Path $root 'packaging\windows'
foreach ($name in @('Install-OwO.cmd', 'Uninstall-OwO.cmd', 'Start-OwO.cmd',
                    'Open-OwO-Settings.cmd',
                    'README.zh-CN.md', 'THIRD-PARTY-NOTICES.md')) {
    Copy-Item -LiteralPath (Join-Path $packaging $name) -Destination $stage -Force
}
foreach ($name in @('Install-OwO.ps1', 'Uninstall-OwO.ps1', 'Finish-Uninstall-OwO.ps1',
                    'Start-OwO.ps1')) {
    Copy-Item -LiteralPath (Join-Path $packaging $name) -Destination (Join-Path $stage 'scripts') -Force
}
Copy-Item -LiteralPath (Join-Path $root 'LICENSE') `
    -Destination (Join-Path $stage 'LICENSES\OwO-GPL-3.0-only.txt') -Force

$licenseInputs = [ordered]@{
    'libime-LGPL-2.1-or-later.txt' = Join-Path $runtime 'licenses\libime-LGPL-2.1-or-later.txt'
    'libcxx-LICENSE.txt' = 'C:\msys64\clang64\share\licenses\libc++\LICENSE'
    'zstd-LICENSE.txt' = 'C:\msys64\clang64\share\licenses\zstd\LICENSE'
    'libiconv-COPYING.txt' = 'C:\msys64\clang64\share\licenses\libiconv\COPYING'
    'libiconv-COPYING.LIB.txt' = 'C:\msys64\clang64\share\licenses\libiconv\COPYING.LIB'
    'gettext-runtime-COPYING.txt' = 'C:\msys64\clang64\share\licenses\gettext-runtime\COPYING'
    'gettext-runtime-COPYING.LIB.txt' = 'C:\msys64\clang64\share\licenses\gettext-runtime\intl\COPYING.LIB'
    'libuv-LICENSE.txt' = 'C:\msys64\clang64\share\licenses\libuv\LICENSE'
    'winpthreads-COPYING.txt' = 'C:\msys64\clang64\share\licenses\winpthreads\COPYING'
    'dlfcn-LICENSE.txt' = 'C:\msys64\clang64\share\licenses\dlfcn\LICENSE'
    'dotnet-LICENSE.txt' = 'C:\Program Files\dotnet\LICENSE.txt'
    'dotnet-ThirdPartyNotices.txt' = 'C:\Program Files\dotnet\ThirdPartyNotices.txt'
    'Microsoft-VC-Runtime-Redist.txt' = Join-Path $vcBuildTools 'Licenses\2052\Redist.txt'
    'Microsoft-VC-Runtime-ThirdPartyNotices.txt' = Join-Path $vcBuildTools 'Licenses\2052\ThirdPartyNotices.txt'
    'WindowsAppSDK-WinUI-LICENSE.txt' = Join-Path $env:USERPROFILE '.nuget\packages\microsoft.windowsappsdk.winui\2.3.0\license.txt'
    'WindowsAppSDK-WinUI-NOTICE.txt' = Join-Path $env:USERPROFILE '.nuget\packages\microsoft.windowsappsdk.winui\2.3.0\NOTICE.txt'
    'WindowsAppSDK-Foundation-LICENSE.txt' = Join-Path $env:USERPROFILE '.nuget\packages\microsoft.windowsappsdk.foundation\2.3.5\license.txt'
    'WindowsAppSDK-InteractiveExperiences-LICENSE.txt' = Join-Path $env:USERPROFILE '.nuget\packages\microsoft.windowsappsdk.interactiveexperiences\2.1.3\license.txt'
    'WindowsAppSDK-Runtime-LICENSE.txt' = Join-Path $env:USERPROFILE '.nuget\packages\microsoft.windowsappsdk.runtime\2.3.1\license.txt'
    'WindowsAppSDK-Runtime-NOTICE.txt' = Join-Path $env:USERPROFILE '.nuget\packages\microsoft.windowsappsdk.runtime\2.3.1\NOTICE.txt'
    'WebView2-LICENSE.txt' = Join-Path $env:USERPROFILE '.nuget\packages\microsoft.web.webview2\1.0.3719.77\LICENSE.txt'
    'WebView2-NOTICE.txt' = Join-Path $env:USERPROFILE '.nuget\packages\microsoft.web.webview2\1.0.3719.77\NOTICE.txt'
}
foreach ($entry in $licenseInputs.GetEnumerator()) {
    if (-not (Test-Path -LiteralPath $entry.Value -PathType Leaf)) {
        throw "Third-party license is missing: $($entry.Value)"
    }
    Copy-Item -LiteralPath $entry.Value -Destination (Join-Path $stage "LICENSES\$($entry.Key)") -Force
}

$hashLines = Get-ChildItem -LiteralPath $stage -File -Recurse |
    Sort-Object FullName | ForEach-Object {
        $relative = $_.FullName.Substring($stage.Length).TrimStart('\', '/').Replace('\', '/')
        $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $_.FullName).Hash.ToLowerInvariant()
        "$hash  $relative"
    }
[IO.File]::WriteAllLines((Join-Path $stage 'SHA256SUMS.txt'), $hashLines,
                         [Text.UTF8Encoding]::new($false))

$binaryZip = Join-Path $releaseRoot "OwO-Input-Method-$Version-windows-x64.zip"
Compress-Archive -LiteralPath $stage -DestinationPath $binaryZip -CompressionLevel Optimal

$sourceStage = Join-Path $releaseRoot "OwO-Input-Method-$Version-source"
[void](New-Item -ItemType Directory -Path $sourceStage -Force)
foreach ($directory in @('apps', 'benchmarks', 'data', 'docs', 'include', 'packaging',
                         'scripts', 'src', 'tests', 'tools')) {
    $source = Join-Path $root $directory
    if (Test-Path -LiteralPath $source) {
        Copy-Item -LiteralPath $source -Destination $sourceStage -Recurse -Force
    }
}
foreach ($name in @('CMakeLists.txt', 'CMakePresets.json', 'global.json', 'LICENSE',
                    'README.md', '项目计划.md')) {
    $source = Join-Path $root $name
    if (Test-Path -LiteralPath $source) { Copy-Item -LiteralPath $source -Destination $sourceStage -Force }
}
Get-ChildItem -LiteralPath $sourceStage -Directory -Recurse |
    Where-Object { $_.Name -in @('bin', 'obj') } |
    Sort-Object FullName -Descending | Remove-Item -Recurse -Force
$sourceZip = Join-Path $releaseRoot "OwO-Input-Method-$Version-source.zip"
Compress-Archive -LiteralPath $sourceStage -DestinationPath $sourceZip -CompressionLevel Optimal

$releaseHashes = foreach ($file in @($binaryZip, $sourceZip)) {
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $file).Hash.ToLowerInvariant()
    "$hash  $([IO.Path]::GetFileName($file))"
}
[IO.File]::WriteAllLines((Join-Path $releaseRoot 'SHA256SUMS.txt'), $releaseHashes,
                         [Text.UTF8Encoding]::new($false))
Write-Output $releaseRoot
