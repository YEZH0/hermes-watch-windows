@echo off
rem Double-click entry point for run-hermesfix.
rem Windows blocks .ps1 by default; this wrapper bypasses the execution policy
rem for this process only (it does not change any system setting).
setlocal
cd /d "%~dp0"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\run-hermesfix.ps1" %*
set CODE=%ERRORLEVEL%
echo.
pause
exit /b %CODE%
