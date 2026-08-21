# gate.ps1 — R5 一键回归门禁（主控收尾使用；R5 Agent 3 交付；R8 增加 UTF-8 校验与 workspace 全量测试开关）
# 顺序：UTF-8 校验 → fmt → clippy → test（默认 -p owo-agent-server；-WorkspaceTests 跑全量）→ node --check → 可选真实 eval。
# 参数：
#   -SkipClippy     跳过 clippy
#   -SkipTest       跳过 cargo test
#   -SkipNode       跳过 node --check
#   -SkipUtf8       跳过 UTF-8 扫描
#   -WorkspaceTests 跑 cargo test --workspace（默认只跑 server）
#   -WithRealEval   追加运行真实模型 eval（需 OPENAI_API_KEY；POST /eval/gate/run）
#   -ServerPort     服务端口（-WithRealEval 时用于 curl，默认 4098）
# 汇总退出码：任一失败打印失败清单并以非零退出。
param(
    [switch]$SkipClippy,
    [switch]$SkipTest,
    [switch]$SkipNode,
    [switch]$SkipUtf8,
    [switch]$WorkspaceTests,
    [switch]$WithRealEval,
    [int]$ServerPort = 4098
)

$ErrorActionPreference = "Continue"
$env:PATH = "C:\Users\23843\.cargo\bin;" + $env:PATH
$root = Split-Path -Parent $PSScriptRoot   # agent-sdk/
$failures = @()
$steps = @()

function Run-Step {
    param([string]$Name, [scriptblock]$Block, [string]$Cwd)
    Write-Host "==> $Name" -ForegroundColor Cyan
    Push-Location $Cwd
    try {
        & $Block
        # $LASTEXITCODE 可能为 $null（本步未运行外部程序）；$null -ne 0 会误判失败。
        $exitCode = $LASTEXITCODE
        if ($null -ne $exitCode -and $exitCode -ne 0) {
            $script:failures += $Name
            Write-Host "    [FAIL] $Name" -ForegroundColor Red
        } else {
            Write-Host "    [OK]   $Name" -ForegroundColor Green
        }
    } catch {
        $script:failures += $Name
        Write-Host "    [FAIL] $Name : $_" -ForegroundColor Red
    } finally {
        Pop-Location
    }
    $script:steps += $Name
}

# 0) UTF-8 扫描（AGENTS.md 硬性要求：所有源文件 UTF-8；ps1 需 BOM）
if (-not $SkipUtf8) {
    Run-Step "UTF-8 校验（rs/ts/js/html/json/css/md/ps1）" {
        $bad = @()
        $exts = @(".rs", ".ts", ".js", ".html", ".json", ".css", ".md")
        $files = Get-ChildItem $root -Recurse -File |
            Where-Object { $_.FullName -notmatch "\\target\\" -and $_.FullName -notmatch "\\node_modules\\" -and $_.FullName -notmatch "\\.git\\" } |
            Where-Object { $exts -contains $_.Extension -or $_.Extension -eq ".ps1" }
        $strict = New-Object System.Text.UTF8Encoding($false, $true)
        foreach ($f in $files) {
            try {
                $bytes = [System.IO.File]::ReadAllBytes($f.FullName)
                $null = $strict.GetString($bytes)
                if ($f.Extension -eq ".ps1") {
                    if (-not ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF)) {
                        $bad += "$($f.FullName)（.ps1 需带 UTF-8 BOM）"
                    }
                }
            } catch {
                $bad += "$($f.FullName)（$($_.Exception.Message)）"
            }
        }
        Write-Host "    [utf8] scanned=$($files.Count) bad=$($bad.Count)"
        if ($bad.Count -gt 0) {
            Write-Host "    UTF-8 违规文件：" -ForegroundColor Red
            foreach ($b in $bad) { Write-Host "      - $b" -ForegroundColor Red }
            exit 1
        }
    } $root
}

# 1) fmt
Run-Step "cargo fmt --all -- --check" {
    cargo fmt --all -- --check 2>&1
} $root

# 2) clippy（workspace 全量；--all-targets）
if (-not $SkipClippy) {
    Run-Step "cargo clippy --workspace --all-targets -- -D warnings" {
        cargo clippy --workspace --all-targets -- -D warnings 2>&1
    } $root
}

# 3) test（默认 server 全量；-WorkspaceTests 时跑 workspace 全量）
if (-not $SkipTest) {
    if ($WorkspaceTests) {
        Run-Step "cargo test --workspace" {
            cargo test --workspace 2>&1
        } $root
    } else {
        Run-Step "cargo test -p owo-agent-server" {
            cargo test -p owo-agent-server 2>&1
        } $root
    }
}

# 4) node --check（app.js + 全部面板）
if (-not $SkipNode) {
    $jsFiles = @("$root\desktop\web\app.js") + (Get-ChildItem "$root\desktop\web\panels" -Filter *.panel.js | ForEach-Object { $_.FullName })
    Run-Step "node --check ($($jsFiles.Count) 个 JS 文件)" {
        $bad = $false
        foreach ($f in $jsFiles) {
            node --check $f 2>&1 | Out-Host
            if ($LASTEXITCODE -ne 0) { $bad = $true }
        }
        if ($bad) { exit 1 }
    } $root
}

# 5) 可选真实 eval（走 HTTP 面，验证 /eval/gate/run 链路）
if ($WithRealEval) {
    if (-not $env:OPENAI_API_KEY) {
        Write-Host "==> 真实 eval 跳过：缺少 OPENAI_API_KEY" -ForegroundColor Yellow
    } else {
        Run-Step "真实 eval：POST /eval/gate/run（服务需已启动于 127.0.0.1:$ServerPort）" {
            $resp = curl.exe --max-time 300 -s -X POST "http://127.0.0.1:$ServerPort/eval/gate/run" -H "Content-Type: application/json" -d "{}"
            Write-Host $resp
            $json = $resp | ConvertFrom-Json
            if (-not $json.ok -and -not $json.skipped) { exit 1 }
        } $root
    }
}

# 汇总
Write-Host ""
Write-Host "==== 门禁汇总（$($steps.Count) 步）====" -ForegroundColor Cyan
foreach ($s in $steps) {
    $mark = if ($failures -contains $s) { "X" } else { "v" }
    Write-Host "  [$mark] $s"
}
if ($failures.Count -gt 0) {
    Write-Host "失败清单：" -ForegroundColor Red
    foreach ($f in $failures) { Write-Host "  - $f" -ForegroundColor Red }
    exit 1
}
Write-Host "全部通过" -ForegroundColor Green
exit 0
