# 内置技能包门禁：documents / spreadsheets / pdf / browser 端到端契约测试。
# 用法：powershell -ExecutionPolicy Bypass -File scripts\skill-gate.ps1
# 可用环境变量覆盖工具链：OWO_SKILL_PYTHON / OWO_SKILL_NODE / OWO_SKILL_PDFTOPPM / OWO_SKILL_RUNTIME
param()

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$runtimeRoot = if ($env:OWO_SKILL_RUNTIME) {
    $env:OWO_SKILL_RUNTIME
} else {
    "C:\Users\23843\.cache\codex-runtimes\codex-primary-runtime\dependencies"
}
$python = if ($env:OWO_SKILL_PYTHON) { $env:OWO_SKILL_PYTHON } else { Join-Path $runtimeRoot "python\python.exe" }
$node = if ($env:OWO_SKILL_NODE) { $env:OWO_SKILL_NODE } else { Join-Path $runtimeRoot "node\bin\node.exe" }
$pdftoppmExe = Join-Path $runtimeRoot "native\poppler\Library\bin\pdftoppm.exe"
$pdftoppm = if ($env:OWO_SKILL_PDFTOPPM) {
    $env:OWO_SKILL_PDFTOPPM
} elseif (Test-Path $pdftoppmExe) {
    $pdftoppmExe
} else {
    Join-Path $runtimeRoot "bin\override\pdftoppm.cmd"
}
if (-not $env:NODE_PATH) {
    $env:NODE_PATH = Join-Path $runtimeRoot "node\node_modules"
}
$env:PYTHONIOENCODING = "utf-8"
$env:PDFTOPPM = $pdftoppm

foreach ($tool in @($python, $node)) {
    if (-not (Test-Path $tool)) {
        Write-Host "[skill-gate] 缺少工具：$tool（可设置 OWO_SKILL_* 覆盖）" -ForegroundColor Red
        exit 1
    }
}

$skillsRoot = Join-Path (Split-Path $PSScriptRoot -Parent) "skills"
$cases = @(
    @{ Name = "documents"; Cmd = $python; Args = @("tests\run_tests.py") },
    @{ Name = "spreadsheets"; Cmd = $python; Args = @("tests\run_tests.py") },
    @{ Name = "pdf"; Cmd = $python; Args = @("tests\run_tests.py") },
    @{ Name = "browser"; Cmd = $node; Args = @("tests\run_tests.js") }
)

$failed = 0
foreach ($case in $cases) {
    $dir = Join-Path $skillsRoot $case.Name
    if (-not (Test-Path $dir)) {
        Write-Host "[skill-gate] $($case.Name) 目录不存在" -ForegroundColor Red
        $failed++
        continue
    }
    Push-Location $dir
    try {
        & $case.Cmd @($case.Args) 2>&1 | ForEach-Object { Write-Host "  $_" }
        if ($LASTEXITCODE -ne 0) { throw "退出码 $LASTEXITCODE" }
        Write-Host "[skill-gate] $($case.Name) PASS" -ForegroundColor Green
    } catch {
        Write-Host "[skill-gate] $($case.Name) FAIL: $_" -ForegroundColor Red
        $failed++
    } finally {
        Pop-Location
    }
}

if ($failed -gt 0) {
    Write-Host "[skill-gate] $failed 个技能未通过" -ForegroundColor Red
    exit 1
}
Write-Host "[skill-gate] 全部内置技能 PASS" -ForegroundColor Green
