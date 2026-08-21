# ci-shared.ps1 — CI 脚本公共助手（供 ci-gate.ps1 / ci-nightly.ps1 / ci-weekly.ps1 点源引用）
# 本文件只提供函数与状态，不直接执行外部命令。
# 注意：保持 Windows PowerShell 5.1 兼容语法（仓库规则），同时兼容 PowerShell 7。
# 用法：. (Join-Path $PSScriptRoot "ci-shared.ps1")
#
# 约定：
#   - $script:ciFailures = List[PSCustomObject{name, detail}]；$script:ciSteps = List[string]
#   - $script:ciStepFilter：-Step 过滤词（子串匹配，空=全部执行）
#   - Invoke-CiStep 块内 `return` 表示"受控跳过"（绕过失败标记并在日志说明 SKIP 原因）

function Get-CiRepoRoot {
    # ci-shared.ps1 与 ci-*.ps1 均位于 agent-sdk/scripts/，根目录即上一级
    return Split-Path -Parent $PSScriptRoot
}

function Initialize-CiPath {
    # 保证 cargo 与 npm 可解析；找不到时回退到用户标准安装目录（不写死单机路径）
    $CARGO_HOME = $env:CARGO_HOME
    if (-not $CARGO_HOME) { $CARGO_HOME = Join-Path $env:USERPROFILE ".cargo" }
    $cargoBin = Join-Path $CARGO_HOME "bin"
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue) -and (Test-Path (Join-Path $cargoBin "cargo.exe"))) {
        $env:PATH = "$cargoBin;" + $env:PATH
    }
    if (-not (Get-Command npm -ErrorAction SilentlyContinue) -and (Test-Path (Join-Path $env:ProgramFiles "nodejs\npm.cmd"))) {
        $env:PATH = "$env:ProgramFiles\nodejs;" + $env:PATH
    }
}

function New-CiFailureState {
    if ($null -eq $script:ciFailures) { $script:ciFailures = @() }
    if ($null -eq $script:ciSteps) { $script:ciSteps = @() }
}

function Add-CiFailure {
    param([Parameter(Mandatory = $true)][string]$Name, [string]$Detail)
    New-CiFailureState
    $script:ciFailures += [pscustomobject]@{ name = $Name; detail = $Detail }
}

# 断言工具可用；不可用时 -Required 抛错（CI 中安全类门禁必须真实执行），否则返回 $false 供调用方显式 SKIP。
function Assert-CiTool {
    param(
        [Parameter(Mandatory = $true)][string]$Tool,
        [string]$InstallHint = "CI 应通过 taiki-e/install-action 安装",
        [switch]$Required
    )
    if (Get-Command $Tool -ErrorAction SilentlyContinue) { return $true }
    if ($Required) {
        throw "$Tool 不可用（$InstallHint）；安全类门禁不可跳过"
    }
    return $false
}

# 执行单个 CI 步骤：输出回显 + 可选落盘 $LogDir\<id>.log；支持 -Step 过滤。
function Invoke-CiStep {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$Id,
        [Parameter(Mandatory = $true)][scriptblock]$Block,
        [string]$Cwd = "",
        [string]$LogDir = ""
    )
    New-CiFailureState
    if ($script:ciStepFilter -and -not $Id.Contains($script:ciStepFilter)) {
        return
    }
    $script:ciSteps += $Name
    Write-Host ("==> {0}" -f $Name) -ForegroundColor Cyan
    $logFile = ""
    if ($LogDir) {
        New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
        $logFile = Join-Path $LogDir ($Id + ".log")
    }
    $outText = ""
    if ($Cwd) { Push-Location $Cwd }
    try {
        $global:LASTEXITCODE = $null
        $outText = (& $Block 2>&1 | Out-String)
        $exit = $global:LASTEXITCODE
        if ($null -eq $exit) { $exit = 0 }
        if ($outText) {
            $trimmed = $outText.TrimEnd("`r", "`n")
            if ($trimmed) { Write-Host $trimmed }
        }
        if ($logFile) { $outText | Out-File -LiteralPath $logFile -Encoding UTF8 }
        if ($exit -ne 0) {
            Add-CiFailure $Name ("exit = {0}" -f $exit)
            Write-Host ("    [FAIL] {0} (exit={1})" -f $Name, $exit) -ForegroundColor Red
        } else {
            Write-Host ("    [OK]   {0}" -f $Name) -ForegroundColor Green
        }
    } catch {
        $err = "EXCEPTION: $($_.Exception.Message)`n$($_.ScriptStackTrace)"
        if ($outText) { $err = $outText + $err }
        Write-Host $err -ForegroundColor Red
        if ($logFile) { $err | Out-File -LiteralPath $logFile -Encoding UTF8 }
        Add-CiFailure $Name $_.Exception.Message
        Write-Host ("    [FAIL] {0} : {1}" -f $Name, $_.Exception.Message) -ForegroundColor Red
    } finally {
        if ($Cwd) { Pop-Location }
    }
}

# 汇总并退出：0 = 全过（含受控 SKIP）；1 = 存在失败（含必装工具缺失）。
function Write-CiSummary {
    param([string]$LogDir = "")
    New-CiFailureState
    $fail = @($script:ciFailures)
    $steps = @($script:ciSteps)
    Write-Host ""
    Write-Host ("==== CI 汇总（{0} 步，{1} 失败）====" -f $steps.Count, $fail.Count) -ForegroundColor Cyan
    foreach ($s in $steps) {
        $matches = @($fail | Where-Object { $_.name -eq $s })
        $mark = if ($matches.Count -gt 0) { "X" } else { "v" }
        Write-Host ("  [{0}] {1}" -f $mark, $s)
    }
    foreach ($f in $fail) {
        Write-Host ("  - FAIL {0} : {1}" -f $f.name, $f.detail) -ForegroundColor Red
    }
    $summary = [ordered]@{
        timestamp = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
        ok        = ($fail.Count -eq 0)
        steps     = $steps
        failures  = @($fail | ForEach-Object { "{0}: {1}" -f $_.name, $_.detail })
    }
    if ($LogDir) {
        New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
        $summaryJson = $summary | ConvertTo-Json -Depth 4
        $summaryJson | Set-Content -LiteralPath (Join-Path $LogDir "summary.json") -Encoding UTF8
    }
    if ($fail.Count -gt 0) {
        Write-Host "存在失败步骤，退出码 1" -ForegroundColor Red
        exit 1
    }
    Write-Host "全部通过" -ForegroundColor Green
    exit 0
}