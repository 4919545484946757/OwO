param(
    [string]$DependencyRoot = (Join-Path $PSScriptRoot "..\build\dependencies")
)

$ErrorActionPreference = "Stop"
$assetId = "501261436"
$assetName = "chinese-addons-any-20260804.tar.bz2"
$assetSha256 = "e3158a51ab3026bca7823d89e99e27c30d8a51e723f300b516caf5d94525b139"
$modelSha256 = "3588b3942c8fd62e1a6bd3bae8c7cadc0faf928c175b245224ed475862218387"
$resolvedRoot = [System.IO.Path]::GetFullPath($DependencyRoot)
$packageRoot = Join-Path $resolvedRoot "libime-1.1.15\model-20260804"
$downloadRoot = Join-Path $resolvedRoot "downloads"
$archive = Join-Path $downloadRoot $assetName
$model = Join-Path $packageRoot "lib\libime\zh_CN.lm"
$assetReceipt = Join-Path $packageRoot "source-archive.sha256"
$modelReceipt = Join-Path $packageRoot "zh_CN.lm.sha256"

if (Test-Path -LiteralPath $model -PathType Leaf) {
    $actualModel = (Get-FileHash -Algorithm SHA256 -LiteralPath $model).Hash.ToLowerInvariant()
    if ($actualModel -ne $modelSha256) {
        throw "Existing libime model SHA-256 mismatch: expected $modelSha256, got $actualModel"
    }
    [System.IO.File]::WriteAllText($modelReceipt, $modelSha256 + [Environment]::NewLine,
                                  [System.Text.UTF8Encoding]::new($false))
    if (Test-Path -LiteralPath $archive -PathType Leaf) {
        $actualArchive = (Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash.ToLowerInvariant()
        if ($actualArchive -eq $assetSha256) {
            [System.IO.File]::WriteAllText($assetReceipt, $assetSha256 + [Environment]::NewLine,
                                          [System.Text.UTF8Encoding]::new($false))
        }
    }
    Write-Output $model
    exit 0
}

New-Item -ItemType Directory -Path $downloadRoot -Force | Out-Null
$assetUrl = "https://api.github.com/repos/fcitx-contrib/fcitx5-plugins/releases/assets/$assetId"
& curl.exe -L --fail --retry 5 --retry-delay 2 --continue-at - `
    -H "Accept: application/octet-stream" -o $archive $assetUrl
if ($LASTEXITCODE -ne 0) {
    throw "Unable to download the pinned Fcitx Chinese model asset"
}
$actualArchive = (Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash.ToLowerInvariant()
if ($actualArchive -ne $assetSha256) {
    throw "Fcitx Chinese model asset SHA-256 mismatch: expected $assetSha256, got $actualArchive"
}

$work = Join-Path $resolvedRoot ("libime-model-extract-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $work | Out-Null
try {
    tar -xjf $archive -C $work "lib/libime/zh_CN.lm"
    if ($LASTEXITCODE -ne 0) {
        throw "Unable to extract zh_CN.lm from the verified asset"
    }
    $extractedModel = Join-Path $work "lib\libime\zh_CN.lm"
    $actualModel = (Get-FileHash -Algorithm SHA256 -LiteralPath $extractedModel).Hash.ToLowerInvariant()
    if ($actualModel -ne $modelSha256) {
        throw "Extracted zh_CN.lm SHA-256 mismatch: expected $modelSha256, got $actualModel"
    }
    New-Item -ItemType Directory -Path (Split-Path -Parent $model) -Force | Out-Null
    Move-Item -LiteralPath $extractedModel -Destination $model
    [System.IO.File]::WriteAllText($assetReceipt, $assetSha256 + [Environment]::NewLine,
                                  [System.Text.UTF8Encoding]::new($false))
    [System.IO.File]::WriteAllText($modelReceipt, $modelSha256 + [Environment]::NewLine,
                                  [System.Text.UTF8Encoding]::new($false))
    Write-Output $model
} finally {
    if (Test-Path -LiteralPath $work) {
        Remove-Item -Recurse -Force -LiteralPath $work
    }
}
