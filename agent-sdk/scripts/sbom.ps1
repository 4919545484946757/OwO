# SBOM generator: workspace dependency manifest (cargo metadata) + release artifact model-file hashes (sha256).
# Output is SPDX-style JSON: { bomFormat, specVersion, version, name, dependencies[], modelFiles[] }.
# Usage: powershell -ExecutionPolicy Bypass -File scripts\sbom.ps1 [-DistDir dist\OwO-Agent] [-OutFile dist\sbom.json]
param(
    # Release artifact directory (source of model-file hashes; skipped when empty).
    [string]$DistDir = "",
    [string]$OutFile = ""
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$root = Split-Path $PSScriptRoot -Parent
if (-not $OutFile) { $OutFile = Join-Path $root "dist\sbom.json" }

$cargo = if ($env:OWO_CARGO) {
    $env:OWO_CARGO
} elseif (Test-Path (Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe")) {
    Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
} else {
    "cargo"
}

# 1) Dependency manifest from cargo metadata (workspace packages).
Write-Host "[sbom] Parsing workspace dependencies (cargo metadata)..."
Push-Location $root
try {
    $metaJson = & $cargo metadata --no-deps --format-version 1 2>&1 | Out-String
} finally {
    Pop-Location
}
$dependencies = @()
if ($LASTEXITCODE -eq 0) {
    $meta = $metaJson | ConvertFrom-Json
    foreach ($package in $meta.packages) {
        $dependencies += [pscustomobject]@{
            name    = $package.name
            version = $package.version
            source  = if ($package.source) { $package.source } else { "workspace" }
        }
    }
}
Write-Host "[sbom] Dependencies: $($dependencies.Count)"

# 2) Artifact hashes (sha256), skip files over 512MB to avoid hangs.
$modelFiles = @()
if ($DistDir -and (Test-Path $DistDir)) {
    Write-Host "[sbom] Hashing artifacts ($DistDir)..."
    $candidates = Get-ChildItem -LiteralPath $DistDir -Recurse -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match "\.(onnx|bin|dll|exe)$" -or $_.FullName -match "\\models\\" }
    foreach ($file in $candidates) {
        if ($file.Length -gt 512MB) { continue }
        try {
            $hash = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
            $relative = $file.FullName.Substring((Resolve-Path $DistDir).Path.Length).TrimStart('\')
            $modelFiles += [pscustomobject]@{
                path   = $relative
                size   = $file.Length
                sha256 = $hash
            }
        } catch {
            Write-Host "[sbom] Hash failed (skipped): $($file.FullName)"
        }
    }
}
Write-Host "[sbom] Artifact hashes: $($modelFiles.Count)"

$sbom = [ordered]@{
    bomFormat    = "SPDX"
    specVersion  = "2.3"
    name         = "OwO-Agent"
    created      = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    dependencies = @($dependencies | Sort-Object name, version)
    modelFiles   = @($modelFiles | Sort-Object path)
}

$dir = Split-Path $OutFile -Parent
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$sbom | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $OutFile -Encoding UTF8
Write-Host "[sbom] Done: $OutFile (deps $($dependencies.Count) / hashes $($modelFiles.Count))"
