@echo off
setlocal
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\start-dev.ps1" -RestartCore -OpenSettings -OpenNotepad
if errorlevel 1 (
  echo.
  echo OwO startup failed. Review the error above.
  pause
)
endlocal
