param(
    [string]$DependencyRoot = (Join-Path $PSScriptRoot "..\build\dependencies"),
    [string]$Msys2Root = "C:\msys64"
)

$ErrorActionPreference = "Stop"
$resolvedRoot = [System.IO.Path]::GetFullPath($DependencyRoot)
$runtime = Join-Path $resolvedRoot "libime-1.1.15\runtime-x86_64"
$bridgeBuild = Join-Path $resolvedRoot "libime-1.1.15\bridge-x86_64"
$fcitxUtils = Join-Path $resolvedRoot "fcitx5-utils-5.1.20\stage"
$clangBin = Join-Path ([System.IO.Path]::GetFullPath($Msys2Root)) "clang64\bin"
$libimeSource = Join-Path $resolvedRoot "libime-1.1.15\source"

$files = [ordered]@{
    "owo_libime_bridge.dll" = Join-Path $bridgeBuild "owo_libime_bridge.dll"
    "libIMECore.dll" = Join-Path $bridgeBuild "bin\libIMECore.dll"
    "libFcitx5Utils.dll" = Join-Path $fcitxUtils "bin\libFcitx5Utils.dll"
    "libc++.dll" = Join-Path $clangBin "libc++.dll"
    "libzstd.dll" = Join-Path $clangBin "libzstd.dll"
    "libdl.dll" = Join-Path $clangBin "libdl.dll"
    "libintl-8.dll" = Join-Path $clangBin "libintl-8.dll"
    "libwinpthread-1.dll" = Join-Path $clangBin "libwinpthread-1.dll"
    "libuv-1.dll" = Join-Path $clangBin "libuv-1.dll"
    "libiconv-2.dll" = Join-Path $clangBin "libiconv-2.dll"
}
foreach ($source in $files.Values) {
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        throw "Required libime runtime input is missing: $source"
    }
}

New-Item -ItemType Directory -Path $runtime -Force | Out-Null
$manifestFiles = [ordered]@{}
foreach ($entry in $files.GetEnumerator()) {
    $destination = Join-Path $runtime $entry.Key
    Copy-Item -LiteralPath $entry.Value -Destination $destination -Force
    $manifestFiles[$entry.Key] =
        (Get-FileHash -Algorithm SHA256 -LiteralPath $destination).Hash.ToLowerInvariant()
}

$licenseSource = Join-Path $libimeSource "LICENSES\LGPL-2.1-or-later.txt"
if (-not (Test-Path -LiteralPath $licenseSource -PathType Leaf)) {
    throw "libime LGPL license is missing: $licenseSource"
}
$licenseDirectory = Join-Path $runtime "licenses"
New-Item -ItemType Directory -Path $licenseDirectory -Force | Out-Null
Copy-Item -LiteralPath $licenseSource `
    -Destination (Join-Path $licenseDirectory "libime-LGPL-2.1-or-later.txt") -Force

$manifest = [ordered]@{
    schema_version = 1
    architecture = "x86_64"
    libime_version = "1.1.15"
    fcitx5_utils_version = "5.1.20"
    bridge_abi_version = 1
    files = $manifestFiles
}
$manifestJson = $manifest | ConvertTo-Json -Depth 4
[System.IO.File]::WriteAllText((Join-Path $runtime "runtime-manifest.json"),
                               $manifestJson + [Environment]::NewLine,
                               [System.Text.UTF8Encoding]::new($false))
Write-Output $runtime

