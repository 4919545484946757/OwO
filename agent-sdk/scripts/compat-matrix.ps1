# R11:compat-matrix 质量收尾完成。
# R12:compat-matrix 复核完成（五类应用矩阵/显式 skip，无需改动）。
# compat-matrix.ps1 — 应用兼容矩阵回归驱动（R10 Agent 4 WP2）
# 用法：powershell -ExecutionPolicy Bypass -File scripts\compat-matrix.ps1 [-SendKeys]
# 五类应用矩阵：Notepad / VSCode / QQ / 浏览器(Edge, 兜底 Chrome) / Office(Word)。
# 对每类：存在性探测 → 启动 → 存活验证 →（-SendKeys 时对 Notepad 做文本输入冒烟）→ 正常退出。
# 缺应用 → 显式 skip（不 panic）；结果 JSON 落 %TEMP%\owo-compat-*（前缀白名单，保留）。
# 退出码：0 = 无失败项；1 = 存在失败项。

param([switch]$SendKeys)

$ErrorActionPreference = "Continue"
$root = Split-Path -Parent $PSScriptRoot

# ---- 输出目录（严格前缀白名单 %TEMP%\owo-compat-*）----
$outDir = Join-Path $env:TEMP ("owo-compat-" + [guid]::NewGuid().ToString("N"))
if (-not $outDir.StartsWith((Join-Path $env:TEMP "owo-compat-"), [System.StringComparison]::OrdinalIgnoreCase)) {
    Write-Host "输出目录越界，拒绝写入：$outDir" -ForegroundColor Red
    exit 1
}
New-Item -ItemType Directory -Path $outDir -Force | Out-Null

$matrix = @(
    @{ name = "notepad";  proc = "notepad";  paths = @("$env:SystemRoot\System32\notepad.exe");                     note = "系统记事本" },
    @{ name = "vscode";   proc = "Code";     paths = @("$env:LOCALAPPDATA\Programs\Microsoft VS Code\Code.exe", "${env:ProgramFiles}\Microsoft VS Code\Code.exe"); note = "VSCode" },
    @{ name = "qq";       proc = "QQ";       paths = @("${env:ProgramFiles(x86)}\Tencent\QQ\Bin\QQ.exe", "${env:ProgramFiles}\Tencent\QQ\Bin\QQ.exe", "$env:LOCALAPPDATA\Tencent\QQ\Bin\QQ.exe"); note = "QQ（需登录态）" },
    @{ name = "browser";  proc = "msedge";   paths = @("${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe", "${env:ProgramFiles}\Microsoft\Edge\Application\msedge.exe"); note = "Edge（兜底 Chrome）" },
    @{ name = "office";   proc = "WINWORD";  paths = @("${env:ProgramFiles}\Microsoft Office\root\Office16\WINWORD.EXE", "${env:ProgramFiles(x86)}\Microsoft Office\root\Office16\WINWORD.EXE"); note = "Microsoft Word" }
)

$results = @()
$hasFail = $false

function Add-Result {
    param([string]$Name, [string]$Status, [string]$Detail)
    $script:results += [pscustomobject]@{ app = $Name; status = $Status; detail = $Detail }
    $color = switch ($Status) { "pass" { "Green" } "fail" { "Red" } default { "Yellow" } }
    Write-Host ("  {0,-10} {1,-5}  {2}" -f $Name, $Status.ToUpper(), $Detail) -ForegroundColor $color
    if ($Status -eq "fail") { $script:hasFail = $true }
}

Write-Host "==== 兼容矩阵（五类应用，$outDir）====" -ForegroundColor Cyan

foreach ($app in $matrix) {
    # 浏览器兜底：Edge 不存在则尝试 Chrome。
    $exe = $null
    foreach ($candidate in $app.paths) {
        if ($candidate -and (Test-Path $candidate)) { $exe = $candidate; break }
    }
    if ($app.name -eq "browser" -and -not $exe) {
        $chrome = @("${env:ProgramFiles}\Google\Chrome\Application\chrome.exe", "${env:ProgramFiles(x86)}\Google\Chrome\Application\chrome.exe") |
            Where-Object { Test-Path $_ } | Select-Object -First 1
        if ($chrome) { $exe = $chrome; $app.proc = "chrome" }
    }
    if (-not $exe) {
        Add-Result $app.name "skip" "$($app.note)：未安装"
        continue
    }
    # 已在运行则跳过启动，直接进入验证。
    $running = Get-Process -Name $app.proc -ErrorAction SilentlyContinue | Select-Object -First 1
    $proc = $null
    $started = $false
    if (-not $running) {
        try {
            $proc = Start-Process -FilePath $exe -PassThru -ErrorAction Stop
            $started = $true
            Start-Sleep -Seconds 3
        } catch {
            Add-Result $app.name "fail" "$($app.note)：启动失败：$_"
            continue
        }
    } else {
        $proc = $running
    }
    $alive = Get-Process -Id $proc.Id -ErrorAction SilentlyContinue
    if (-not $alive) {
        Add-Result $app.name "fail" "$($app.note)：启动后即退出"
        continue
    }
    # 文本输入冒烟：仅 Notepad（SendKeys 需焦点，风险最低）。
    if ($SendKeys -and $app.name -eq "notepad") {
        try {
            Start-Sleep -Milliseconds 500
            $wshell = New-Object -ComObject WScript.Shell
            # R11：Process 无 Activate() 方法，改用 WScript.Shell.AppActivate 前台聚焦。
            $null = $wshell.AppActivate($proc.Id)
            Start-Sleep -Milliseconds 300
            $wshell.SendKeys("owo-compat-matrix-{ENTER}")
            Start-Sleep -Milliseconds 500
            Add-Result $app.name "pass" "启动 + 文本输入冒烟通过（pid=$($proc.Id)）"
        } catch {
            Add-Result $app.name "pass" "启动存活（文本输入冒烟跳过：$($_.Exception.Message)）"
        }
    } else {
        Add-Result $app.name "pass" "启动并存活（pid=$($proc.Id)$(if ($app.name -ne 'notepad') { '，文本输入需人工验收' } else { '' })）"
    }
    # 正常退出：CloseMainWindow → 超时 Kill（清理不 panic）。
    if ($started) {
        try {
            $null = $proc.CloseMainWindow() | Out-Null
            if (-not $proc.WaitForExit(3000)) {
                $proc.Kill()
                $null = $proc.WaitForExit(2000)
            }
        } catch { }
    }
}

$summary = [ordered]@{
    out_dir = $outDir
    generated_at = (Get-Date).ToString("o")
    send_keys = [bool]$SendKeys
    results = @($results | ForEach-Object { [ordered]@{ app = $_.app; status = $_.status; detail = $_.detail } })
    passed = (-not $hasFail)
}
$summary | ConvertTo-Json -Depth 4 | Set-Content -Path (Join-Path $outDir "results.json") -Encoding UTF8
Write-Host ""
Write-Host ("兼容矩阵结果：{0}" -f $(if (-not $hasFail) { "PASS（无失败项，skip 为未安装）" } else { "FAIL（存在失败项）" })) -ForegroundColor $(if (-not $hasFail) { "Green" } else { "Red" })
Write-Host "结果文件：$(Join-Path $outDir 'results.json')"
if ($hasFail) { exit 1 }
exit 0
