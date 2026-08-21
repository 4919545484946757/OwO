# ci-gate.ps1 — agent-sdk CI 核心门禁（PR/Merge 工作流与本地验证共用；幂等、只读为主）
# 步骤顺序：utf8 → fmt → clippy → test → route-contract → node → ts
# 约定：块内通过退出码或 $global:LASTEXITCODE = 1 表达失败（避免 throw 吞掉已捕获输出）；
#       前置条件缺失用 return 实现受控跳过（绿色 + SKIP 说明）。
# 用法：
#   powershell -ExecutionPolicy Bypass -File scripts\ci-gate.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\ci-gate.ps1 -Step fmt
#   powershell -ExecutionPolicy Bypass -File scripts\ci-gate.ps1 -Step clippy -LogDir "$env:TEMP\ci-gate-logs"
# 参数：
#   -Step <词>           只运行 Id 包含该词的步骤（CI 拆分步骤与本地排查用）
#   -SkipFmt/-SkipClippy/-SkipTest/-SkipNode/-SkipTs/-SkipRouteContract/-SkipUtf8
#   -ServerOnly          workspace 测试退化为只测 owo-agent-server 单包
#   -LogDir <目录>       把每步输出与 summary.json 落盘（诊断 artifact）
# 退出码：0 = 全过；1 = 存在失败步骤。
param(
    [string]$Step = "",
    [switch]$SkipFmt,
    [switch]$SkipClippy,
    [switch]$SkipTest,
    [switch]$SkipNode,
    [switch]$SkipTs,
    [switch]$SkipRouteContract,
    [switch]$SkipUtf8,
    [switch]$ServerOnly,
    [string]$LogDir = ""
)

$ErrorActionPreference = "Continue"
. (Join-Path $PSScriptRoot "ci-shared.ps1")
Initialize-CiPath
$script:ciStepFilter = $Step.ToLowerInvariant()
$root = Get-CiRepoRoot

# 0) UTF-8 扫描（AGENTS.md 硬性要求：源文件 UTF-8；.ps1 需带 UTF-8 BOM）
if (-not $SkipUtf8) {
    Invoke-CiStep -Name "UTF-8 校验（rs/ts/js/html/json/css/md/ps1）" -Id "utf8" -Cwd $root -LogDir $LogDir -Block {
        $bad = @()
        $exts = @(".rs", ".ts", ".js", ".html", ".json", ".css", ".md", ".ps1")
        $files = Get-ChildItem $root -Recurse -File |
            Where-Object { $_.FullName -notmatch "\\target\\" -and $_.FullName -notmatch "\\node_modules\\" -and $_.FullName -notmatch "\\.git\\" } |
            Where-Object { $exts -contains $_.Extension }
        $strict = New-Object System.Text.UTF8Encoding($false, $true)
        foreach ($f in $files) {
            try {
                $bytes = [System.IO.File]::ReadAllBytes($f.FullName)
                $null = $strict.GetString($bytes)
                if ($f.Extension -eq ".ps1" -and -not ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF)) {
                    $bad += "$($f.FullName)（.ps1 需带 UTF-8 BOM）"
                }
            } catch {
                $bad += "$($f.FullName)（$($_.Exception.Message)）"
            }
        }
        Write-Host ("    [utf8] scanned={0} bad={1}" -f $files.Count, $bad.Count)
        if ($bad.Count -gt 0) {
            $badList = $bad | Sort-Object -Unique
            Write-Host "    UTF-8 违规文件：" -ForegroundColor Red
            foreach ($b in $badList) {
                Write-Host ("      - {0}" -f $b) -ForegroundColor Red
                Write-Output $b
            }
            $global:LASTEXITCODE = 1
        } else {
            $global:LASTEXITCODE = 0
        }
    }
}

# 1) fmt
if (-not $SkipFmt) {
    Invoke-CiStep -Name "cargo fmt --all -- --check" -Id "fmt" -Cwd $root -LogDir $LogDir -Block {
        cargo fmt --all -- --check
    }
}

# 2) clippy（workspace 全量；--locked 保证与 Cargo.lock 一致）
if (-not $SkipClippy) {
    Invoke-CiStep -Name "cargo clippy --workspace --all-targets --locked -- -D warnings" -Id "clippy" -Cwd $root -LogDir $LogDir -Block {
        cargo clippy --workspace --all-targets --locked -- -D warnings
    }
}

# 3) test（默认 workspace 全量；-ServerOnly 时只测 owo-agent-server 单包）
if (-not $SkipTest) {
    $testName = if ($ServerOnly) { "cargo test -p owo-agent-server --locked" } else { "cargo test --workspace --locked" }
    $testId = if ($ServerOnly) { "test-server" } else { "test" }
    Invoke-CiStep -Name $testName -Id $testId -Cwd $root -LogDir $LogDir -Block {
        if ($ServerOnly) {
            cargo test -p owo-agent-server --locked
        } else {
            cargo test --workspace --locked
        }
    }
}

# 4) 路由契约（HTTP 契约面，AGENTS.md 要求同步 route_contract_tests）
if (-not $SkipRouteContract) {
    Invoke-CiStep -Name "cargo test -p owo-agent-server --test route_contract_tests --locked" -Id "route-contract" -Cwd $root -LogDir $LogDir -Block {
        cargo test -p owo-agent-server --test route_contract_tests --locked
    }
}

# 5) Node 语法检查（app.js + 全部 panel）
if (-not $SkipNode) {
    Invoke-CiStep -Name "node --check（app.js + panels/*.panel.js）" -Id "node" -Cwd $root -LogDir $LogDir -Block {
        $jsFiles = @(Join-Path $root "desktop\web\app.js")
        $panels = Get-ChildItem (Join-Path $root "desktop\web\panels") -Filter *.panel.js -ErrorAction SilentlyContinue | ForEach-Object { $_.FullName }
        if ($panels) { $jsFiles += @($panels) }
        Write-Host ("    [node] files={0}" -f $jsFiles.Count)
        $bad = $false
        foreach ($f in $jsFiles) {
            node --check $f
            if ($global:LASTEXITCODE -ne 0) { $bad = $true }
        }
        if ($bad) { $global:LASTEXITCODE = 1 } else { $global:LASTEXITCODE = 0 }
    }
}

# 6) TS 类型检查（clients/ts；package-lock.json 锁定依赖）
if (-not $SkipTs) {
    Invoke-CiStep -Name "TS 类型检查（clients\ts npm run typecheck）" -Id "ts" -Cwd $root -LogDir $LogDir -Block {
        Push-Location (Join-Path $root "clients\ts")
        try {
            if (-not (Test-Path (Join-Path $root "clients\ts\node_modules\typescript"))) {
                npm ci --ignore-scripts
                if ($global:LASTEXITCODE -ne 0) { return }
            }
            npm run typecheck
        } finally {
            Pop-Location
        }
    }
}

Write-CiSummary -LogDir $LogDir