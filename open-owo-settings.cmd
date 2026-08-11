@echo off
setlocal
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\open-settings.ps1"
if errorlevel 1 (
  echo.
  echo OwO settings center startup failed. Review the error above.
  pause
)
endlocal
