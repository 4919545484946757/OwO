param(
    [string]$Suite = "",
    [double]$Threshold = 0.8,
    [string]$Binary = ""
)

$ErrorActionPreference = "Stop"

$bin = if ($Binary) { $Binary } else { Join-Path $PSScriptRoot "..\target\debug\owo-agent.exe" }
$evalArgs = @("eval")
if ($Suite) { $evalArgs += @("--suite", $Suite) }

$output = & $bin @evalArgs 2>&1 | Out-String
$start = $output.IndexOf("{")
if ($start -lt 0) {
    Write-Host "Cannot parse eval report. Raw output:"
    Write-Host $output
    exit 2
}

$report = $output.Substring($start) | ConvertFrom-Json
$rate = [double]$report.pass_rate
Write-Host ("Eval: {0}/{1} passed, rate {2:P1} (threshold {3:P1})" -f $report.passed, $report.total, $rate, $Threshold)

if ($rate -ge $Threshold) {
    exit 0
} else {
    Write-Host "Eval gate failed (pass rate below threshold)"
    exit 1
}
