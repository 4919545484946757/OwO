[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$InstallRoot,
    [Parameter(Mandatory = $true)][string]$ProgramRoot,
    [Parameter(Mandatory = $true)][int]$ParentProcessId
)

$ErrorActionPreference = 'Stop'
$install = [IO.Path]::GetFullPath($InstallRoot)
$program = [IO.Path]::GetFullPath($ProgramRoot)
$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$helperRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSCommandPath))

if (-not $install.StartsWith($program + [IO.Path]::DirectorySeparatorChar,
                            [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Refusing to remove a directory outside the OwO program directory.'
}
if (-not $helperRoot.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -or
    -not ([IO.Path]::GetFileName($helperRoot)).StartsWith(
        'OwO-Uninstall-', [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Refusing to run the cleanup helper outside its isolated temporary directory.'
}

Wait-Process -Id $ParentProcessId -Timeout 30 -ErrorAction SilentlyContinue

$removed = $false
for ($attempt = 0; $attempt -lt 40; ++$attempt) {
    try {
        if (Test-Path -LiteralPath $install) {
            Remove-Item -LiteralPath $install -Recurse -Force
        }
        $removed = -not (Test-Path -LiteralPath $install)
        if ($removed) { break }
    } catch {
        if ($attempt -eq 39) { throw }
    }
    Start-Sleep -Milliseconds 250
}
if (-not $removed) { throw "OwO program directory is still in use: $install" }

if (Test-Path -LiteralPath $program -PathType Container) {
    $remaining = @(Get-ChildItem -LiteralPath $program -Force)
    if ($remaining.Count -eq 0) { Remove-Item -LiteralPath $program -Force }
}

# PowerShell has already parsed this helper, so its isolated temporary copy can
# be removed along with the directory after program cleanup succeeds.
Remove-Item -LiteralPath $helperRoot -Recurse -Force -ErrorAction SilentlyContinue
