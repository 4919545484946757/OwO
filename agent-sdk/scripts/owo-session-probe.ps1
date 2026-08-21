# 会话探针：由计划任务（/IT）在交互会话执行，输出会话名与 QQ 进程，用于验证桌面可达性。
$out = Join-Path $env:TEMP "owo-session-probe.txt"
"SESSIONNAME=$env:SESSIONNAME" | Set-Content -Encoding UTF8 $out
tasklist /fi "imagename eq QQ.exe" /fo csv | Add-Content -Encoding UTF8 $out
