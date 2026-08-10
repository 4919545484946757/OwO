@echo off
powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\Uninstall-OwO.ps1" %*
if errorlevel 1 (
    echo OwO uninstall failed. Review the error above.
    pause
    exit /b 1
)
echo OwO uninstall completed. Locked files, if any, will finish cleanup after applications close.
pause
