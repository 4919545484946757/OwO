@echo off
setlocal
set "OWO_SETTINGS=%~dp0settings\OwO.Settings.exe"
if not exist "%OWO_SETTINGS%" (
    echo OwO Settings Center was not found:
    echo %OWO_SETTINGS%
    pause
    exit /b 1
)
start "" "%OWO_SETTINGS%"
exit /b 0
