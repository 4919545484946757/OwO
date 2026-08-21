# ci-weekly.ps1 — Weekly 流水入口：依赖审计 + 真模型 eval/外部验收（BYOK 感知）+ SBOM
# Windows PowerShell 5.1 兼容；只读/受控。
# BYOK 规则：缺少 OPENAI_API_KEY 时对真模型类步骤显式 SKIP，且任何日志都不打印密钥本身或与密钥相关的内容。
# 用法：
#   powershell -ExecutionPolicy Bypass -File scripts\ci-weekly.ps1 -LogDir "$env:TEMP\ci-weekly" -RequireSecurityTools
# 退出码：0 = 全过（含显式 SKIP）；1 = 存在失败。
param(
    [string]$LogDir = "",
    [switch]$SkipAudit,
    [switch]$SkipDeny,
    [switch]$SkipSbom,
    [switch]$SkipExternal,
    [switch]$RequireSecurityTools,
    [int]$ServerPort = 4098,
    [string]$Binary = ""
)

$ErrorActionPreference = "Continue"
. (Join-Path $PSScriptRoot "ci-shared.ps1")
Initialize-CiPath
$script:ciStepFilter = ""
$root = Get-CiRepoRoot
if (-not $LogDir) { $LogDir = Join-Path $env:TEMP ("owo-ci-weekly-" + [guid]::NewGuid().ToString("N")) }
if (-not $Binary) { $Binary = Join-Path $root "target\debug\owo-agent.exe" }

function Test-CiServerReady {
    param([int]$Port)
    if ($Port -le 0) { return $false }
    try {
        $r = Invoke-WebRequest -Uri ("http://127.0.0.1:{0}/health" -f $Port) -UseBasicParsing -TimeoutSec 3
        return ($r.StatusCode -eq 200)
    } catch {
        return $false
    }
}

# 1) RUSTSEC 漏洞扫描（每周刷新公告库基线）
if (-not $SkipAudit) {
    Invoke-CiStep -Name "cargo audit（RUSTSEC 漏洞扫描，每次拉取最新公告库）" -Id "audit" -Cwd $root -LogDir $LogDir -Block {
        if (-not (Assert-CiTool "cargo-audit" -Required:$RequireSecurityTools)) {
            Write-Host "[SKIP] cargo-audit 未安装（本地只读门禁跳过；CI 经安装提供真实阻断）"
            return
        }
        cargo audit --color never
        if ($global:LASTEXITCODE -ne 0) { throw "cargo audit 发现漏洞（exit $global:LASTEXITCODE）" }
    }
}

# 2) cargo deny（许可证 / 禁止 / 来源 / 公告）
if (-not $SkipDeny) {
    Invoke-CiStep -Name "cargo deny check（deny.toml 策略基线）" -Id "deny" -Cwd $root -LogDir $LogDir -Block {
        if (-not (Assert-CiTool "cargo-deny" -Required:$RequireSecurityTools)) {
            Write-Host "[SKIP] cargo-deny 未安装（本地只读门禁跳过；CI 经安装提供真实阻断）"
            return
        }
        cargo deny check --color never
        if ($global:LASTEXITCODE -ne 0) { throw "cargo deny 未通过（exit $global:LASTEXITCODE），请按 deny.toml 过渡规则处理且不得静默忽略高危" }
    }
}

# 3) SBOM（每周产物有一份当下依赖清单）
if (-not $SkipSbom) {
    Invoke-CiStep -Name "生成 SBOM（scripts\sbom.ps1 → dist\sbom.json）" -Id "sbom" -Cwd $root -LogDir $LogDir -Block {
        $sbom = Join-Path $PSScriptRoot "sbom.ps1"
        if (-not (Test-Path $sbom)) { Write-Host "[SKIP] sbom.ps1 不存在"; return }
        & $sbom -OutFile (Join-Path $root "dist\sbom.json")
        if ($global:LASTEXITCODE -ne 0) { throw "SBOM 生成失败" }
    }
}

# 4) 红队/对抗语料入口（占位：仓库内暂无红队语料资产与工具，显式 SKIP，避免静默空跑）
Invoke-CiStep -Name "红队 / 对抗语料样本（占位入口）" -Id "red-team" -Cwd $root -LogDir $LogDir -Block {
    Write-Host "[SKIP] 红队语料资产与工具未在仓库内提供；此入口为占位。接入后把执行脚本放入 agent-sdk/scripts/ 并在 ci-weekly.ps1 此步骤启用。"
}

# 5) 外部验收 + 真模型 eval（BYOK 感知；缺凭据显式 SKIP）
if (-not $SkipExternal) {
    Invoke-CiStep -Name "外部验收 + 真模型 eval（BYOK）" -Id "external-eval" -Cwd $root -LogDir $LogDir -Block {
        if (-not $env:OPENAI_API_KEY) {
            Write-Host "[SKIP] 未配置 OPENAI_API_KEY（BYOK 缺失）：真模型 eval / 外部验收按技术文档 §6.4 Weekly 流程显式跳过；不发起模型调用，日志也不含密钥相关内容。"
            return
        }
        # 外部验收 HTTP 面（external-acceptance.ps1 自带逐项 skip，缺 server 则整体跳过）
        if (Test-CiServerReady -Port $ServerPort) {
            $acc = Join-Path $PSScriptRoot "external-acceptance.ps1"
            if (Test-Path $acc) {
                & $acc -Port $ServerPort
                if ($global:LASTEXITCODE -ne 0) { throw "外部验收存在失败项（不全为 skip）" }
            } else {
                Write-Host "[SKIP] external-acceptance.ps1 不存在"
            }
        } else {
            Write-Host ("[SKIP] 端口 {0} 无运行中的 server：外部验收 HTTP 面跳过（自建 runner 启动 owo-agent serve 后生效）" -f $ServerPort)
        }
        # 真模型 eval gate（需要编译产物）
        if (Test-Path $Binary) {
            $evalGate = Join-Path $PSScriptRoot "run-eval-gate.ps1"
            if (Test-Path $evalGate) {
                & $evalGate -Binary $Binary -Threshold 0.8
                if ($global:LASTEXITCODE -ne 0) { throw "真模型 eval 总体通过率低于 80% 阈值" }
            } else {
                Write-Host "[SKIP] run-eval-gate.ps1 不存在"
            }
        } else {
            Write-Host ("[SKIP] 未找到 owo-agent 可执行文件（{0}），真模型 eval gate 跳过（需先 cargo build）" -f $Binary)
        }
    }
}

Write-CiSummary -LogDir $LogDir