param(
    [string]$DependencyRoot = (Join-Path $PSScriptRoot "..\build\dependencies")
)

$ErrorActionPreference = "Stop"
$version = "1.1.15"
$expectedSha256 = "e8ce7b90035aeafa5ce5f59a05f84d6c192fcecc009b2e74cf179bc18b21eaf5"
$kenlmCommit = "4cb443e60b7bf2c0ddf3c745378f76cb59e254e5"
$kenlmExpectedSha256 = "11df4f929b175f0b3bd26be7a5a83b3c17cdefa83baecab0b6e69f77430184ea"
$resolvedRoot = [System.IO.Path]::GetFullPath($DependencyRoot)
$target = Join-Path $resolvedRoot "libime-$version\source"
$cmakeFile = Join-Path $target "CMakeLists.txt"
$licenseFile = Join-Path $target "LICENSES\LGPL-2.1-or-later.txt"
$receipt = Join-Path (Split-Path -Parent $target) "source-archive.sha256"
$kenlmTarget = Join-Path $target "src\libime\core\kenlm"
$kenlmHeader = Join-Path $kenlmTarget "lm\model.hh"
$kenlmReceipt = Join-Path (Split-Path -Parent $target) "kenlm-source-archive.sha256"

$sourceReady = (Test-Path -LiteralPath $cmakeFile) -and
               (Test-Path -LiteralPath $licenseFile) -and
               (Test-Path -LiteralPath $receipt) -and
               ([System.IO.File]::ReadAllText($receipt).Trim().ToLowerInvariant() -eq $expectedSha256)
$kenlmReady = (Test-Path -LiteralPath $kenlmHeader) -and
              (Test-Path -LiteralPath $kenlmReceipt) -and
              ([System.IO.File]::ReadAllText($kenlmReceipt).Trim().ToLowerInvariant() -eq
               $kenlmExpectedSha256)
if ($sourceReady -and $kenlmReady) {
    Write-Output $target
    exit 0
}
if ((Test-Path -LiteralPath $target) -and -not $sourceReady) {
    throw "Refusing to replace incomplete existing dependency directory: $target"
}

New-Item -ItemType Directory -Path $resolvedRoot -Force | Out-Null
$work = Join-Path $resolvedRoot ("libime-download-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $work | Out-Null
try {
    if (-not $sourceReady) {
        $archive = Join-Path $work "libime-$version.tar.gz"
        Invoke-WebRequest -Uri "https://github.com/fcitx/libime/archive/refs/tags/$version.tar.gz" `
            -OutFile $archive
        $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash.ToLowerInvariant()
        if ($actual -ne $expectedSha256) {
            throw "libime archive SHA-256 mismatch: expected $expectedSha256, got $actual"
        }

        tar -xf $archive -C $work
        if ($LASTEXITCODE -ne 0) {
            throw "Unable to extract the verified libime archive"
        }
        $extracted = Join-Path $work "libime-$version"
        if (-not (Test-Path -LiteralPath (Join-Path $extracted "CMakeLists.txt")) -or
            -not (Test-Path -LiteralPath (Join-Path $extracted "LICENSES\LGPL-2.1-or-later.txt"))) {
            throw "Verified libime archive does not contain the expected source tree and license"
        }

        $targetParent = Split-Path -Parent $target
        New-Item -ItemType Directory -Path $targetParent | Out-Null
        Move-Item -LiteralPath $extracted -Destination $target
        [System.IO.File]::WriteAllText($receipt, $expectedSha256 + [Environment]::NewLine,
                                      [System.Text.UTF8Encoding]::new($false))
    }

    $kenlmArchive = Join-Path $work "kenlm-$kenlmCommit.tar.gz"
    Invoke-WebRequest -Uri "https://github.com/kpu/kenlm/archive/$kenlmCommit.tar.gz" `
        -OutFile $kenlmArchive
    $kenlmActual = (Get-FileHash -Algorithm SHA256 -LiteralPath $kenlmArchive).Hash.ToLowerInvariant()
    if ($kenlmActual -ne $kenlmExpectedSha256) {
        throw "KenLM archive SHA-256 mismatch: expected $kenlmExpectedSha256, got $kenlmActual"
    }
    tar -xf $kenlmArchive -C $work
    if ($LASTEXITCODE -ne 0) {
        throw "Unable to extract the verified KenLM archive"
    }
    $kenlmExtracted = Join-Path $work "kenlm-$kenlmCommit"
    if (-not (Test-Path -LiteralPath (Join-Path $kenlmExtracted "lm\model.hh"))) {
        throw "Verified KenLM archive does not contain the expected source tree"
    }
    if (Test-Path -LiteralPath $kenlmTarget) {
        $existing = Get-ChildItem -LiteralPath $kenlmTarget -Force
        if ($existing.Count -ne 0) {
            throw "Refusing to replace non-empty existing KenLM source: $kenlmTarget"
        }
        Remove-Item -LiteralPath $kenlmTarget -Force
    }
    Move-Item -LiteralPath $kenlmExtracted -Destination $kenlmTarget
    [System.IO.File]::WriteAllText($kenlmReceipt, $kenlmExpectedSha256 + [Environment]::NewLine,
                                  [System.Text.UTF8Encoding]::new($false))
    Write-Output $target
} finally {
    if (Test-Path -LiteralPath $work) {
        Remove-Item -Recurse -Force -LiteralPath $work
    }
}
