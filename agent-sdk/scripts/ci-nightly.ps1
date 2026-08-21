# ci-nightly.ps1 — Nightly 流水入口：依赖安全审计 + 较重测试 + 可控 runtime 入口
# Windows PowerShell 5.1 兼容；除 cargo 构建产物与 %TEMP% 外不产生写入；任何"受控跳过"都明示原因。
# 用法：
#   powershell -ExecutionPolicy Bypass -File scripts\ci-nightly.ps1 -LogDir "$env:TEMP\ci-nightly" `
#       -RequireSecurityTools -IncludeBench
# 运行时入口（soak / fault-inject / sim-e2e / skill-gate / bench）均先探测前置条件，缺失即显式 SKIP，不 panic、不伪造通过。
# 退出码：0 = 全过（含受控 SKIP）；1 = 存在失败。
param(
    [string]$LogDir = "",
    [switch]$SkipAudit,
    [switch]$SkipDeny,
    [switch]$SkipReleaseTest,
    [switch]$SkipSbom,
    [switch]$IncludeBench,
    [switch]$IncludeSoak,
    [switch]$IncludeFaultInject,
    [switch]$IncludeSimE2E,
    [switch]$IncludeSkillGate,
    # CI 下安全类门禁必须真实执行（工具缺失即失败），本地默认宽松（工具缺失显式 SKIP）
    [switch]$RequireSecurityTools,
    [int]$SoakMinutes = 10,
    [int]$ServerPort = 4098
)

$ErrorActionPreference = "Continue"
. (Join-Path $PSScriptRoot "ci-shared.ps1")
Initialize-CiPath
$script:ciStepFilter = ""
$root = Get-CiRepoRoot
if (-not $LogDir) { $LogDir = Join-Path $env:TEMP ("owo-ci-nightly-" + [guid]::NewGuid().ToString("N")) }

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

# 1) RUSTSEC 漏洞扫描
if (-not $SkipAudit) {
    Invoke-CiStep -Name "cargo audit（RUSTSEC 漏洞扫描，读取 Cargo.lock）" -Id "audit" -Cwd $root -LogDir $LogDir -Block {
        if (-not (Assert-CiTool "cargo-audit" -Required:$RequireSecurityTools)) {
            Write-Host "[SKIP] cargo-audit 未安装（本地只读门禁不安装第三方工具；CI 经 taiki-e/install-action 提供并对漏洞真实阻断）"
            return
        }
        cargo audit --color never
        if ($global:LASTEXITCODE -ne 0) { throw "cargo audit 发现漏洞（exit $global:LASTEXITCODE）" }
    }
}

# 2) cargo deny（许可证 / 禁止 / 来源 / 公告；策略基线 deny.toml）
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

# 3) release 模式较重测试
if (-not $SkipReleaseTest) {
    Invoke-CiStep -Name "cargo test --workspace --release --locked" -Id "release-test" -Cwd $root -LogDir $LogDir -Block {
        cargo test --workspace --release --locked
        if ($global:LASTEXITCODE -ne 0) { throw "release 模式测试失败" }
    }
}

# 4) SBOM（依赖清单 + 产物哈希）
if (-not $SkipSbom) {
    Invoke-CiStep -Name "生成 SBOM（scripts\sbom.ps1 → dist\sbom.json）" -Id "sbom" -Cwd $root -LogDir $LogDir -Block {
        $sbom = Join-Path $PSScriptRoot "sbom.ps1"
        if (-not (Test-Path $sbom)) { Write-Host "[SKIP] sbom.ps1 不存在"; return }
        & $sbom -OutFile (Join-Path $root "dist\sbom.json")
        if ($global:LASTEXITCODE -ne 0) { throw "SBOM 生成失败" }
    }
}

# 5) 性能护栏（自包含：自起临时服务，无需用户环境）
if ($IncludeBench) {
    Invoke-CiStep -Name "性能护栏 bench-metrics（/metrics/overview p95，错误率>5% 视为失败）" -Id "bench" -Cwd $root -LogDir $LogDir -Block {
        $bench = Join-Path $PSScriptRoot "bench-metrics.ps1"
        if (-not (Test-Path $bench)) { Write-Host "[SKIP] bench-metrics.ps1 不存在"; return }
        & $bench -Iterations 200
        if ($global:LASTEXITCODE -ne 0) { throw "性能护栏未达标（错误率 > 5%）" }
    }
}

# 6) soak（需要运行中的 server + 想要开启时）
if ($IncludeSoak) {
    Invoke-CiStep -Name ("soak 短模式（" + $SoakMinutes + " 分钟；需运行中的 server :" + $ServerPort + "）") -Id "soak" -Cwd $root -LogDir $LogDir -Block {
        if (-not (Test-CiServerReady -Port $ServerPort)) {
            Write-Host ("[SKIP] 服务 {0} 未就绪；soak 需要运行中的 owo-agent server（自建 runner 场景启用）" -f $ServerPort)
            return
        }
        $soak = Join-Path $PSScriptRoot "soak.ps1"
        & $soak -Port $ServerPort -Minutes $SoakMinutes
        if ($global:LASTEXITCODE -ne 0) { throw "soak 未达标（卡死或资源超限）" }
    }
}

# 7) 故障注入（需要运行中的 server；前置缺失走受控 SKIP，避免误报）
if ($IncludeFaultInject) {
    Invoke-CiStep -Name ("故障注入 fault-inject（需运行中的 server :" + $ServerPort + "）") -Id "fault-inject" -Cwd $root -LogDir $LogDir -Block {
        if (-not (Test-CiServerReady -Port $ServerPort)) {
            Write-Host "[SKIP] 服务未就绪，跳过故障注入（杜绝无服务器下的误报）"
            return
        }
        $fi = Join-Path $PSScriptRoot "fault-inject.ps1"
        & $fi -Port $ServerPort -TimeoutSec 8
        if ($global:LASTEXITCODE -ne 0) { throw "故障注入关键场景失败" }
    }
}

# 8) sim-regression（需要真实模型 BYOK 与本地模拟运行期）
if ($IncludeSimE2E) {
    Invoke-CiStep -Name "sim-regression run-sim-e2e" -Id "sim-e2e" -Cwd $root -LogDir $LogDir -Block {
        if (-not $env:OPENAI_API_KEY) {
            Write-Host "[SKIP] 缺少 OPENAI_API_KEY（BYOK 未配置）：sim-regression 依赖真实模型，显式跳过且不在任何输出中出现密钥。"
            return
        }
        if (-not (Test-Path (Join-Path $root "target\debug\owo-agent.exe"))) {
            Write-Host "[SKIP] target\debug\owo-agent.exe 不存在，sim-regression 需要先构建 debug"
            return
        }
        $sim = Join-Path $PSScriptRoot "run-sim-e2e.ps1"
        & $sim
        if ($global:LASTEXITCODE -ne 0) { throw "sim-regression 未通过" }
    }
}

# 9) 内置技能包门禁（需要技能运行期目录）
if ($IncludeSkillGate) {
    Invoke-CiStep -Name "skill-gate（内置技能包端到端契约）" -Id "skill-gate" -Cwd $root -LogDir $LogDir -Block {
        $sg = Join-Path $PSScriptRoot "skill-gate.ps1"
        if (-not (Test-Path $sg)) { Write-Host "[SKIP] skill-gate.ps1 不存在"; return }
        $runtimeRoot = if ($env:OWO_SKILL_RUNTIME) {
            $env:OWO_SKILL_RUNTIME
        } else {
            Join-Path $env:USERPROFILE ".cache\codex-runtimes\codex-primary-runtime\dependencies"
        }
        if (-not (Test-Path $runtimeRoot)) {
            Write-Host ("[SKIP] 缺少技能运行期（{0} 不存在；自建 runner 可设 OWO_SKILL_RUNTIME 启用）" -f $runtimeRoot)
            return
        }
        & $sg
        if ($global:LASTEXITCODE -ne 0) { throw "skill-gate 未通过" }
    }
}

Write-CiSummary -LogDir $LogDir