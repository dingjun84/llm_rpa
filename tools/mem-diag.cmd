@echo off
rem ====================================================================
rem  mem-diag.cmd  --  launcher for mem-diag.ps1
rem
rem  Double-click it, or run it from cmd:
rem      mem-diag.cmd
rem      mem-diag.cmd -Days 7
rem      mem-diag.cmd -OutDir D:\reports
rem
rem  Read-only: it never changes system settings, never reboots,
rem  never deletes anything. It only writes one report .txt next to
rem  this script.
rem
rem  Kept ASCII-only on purpose: cmd.exe parses .cmd files with the
rem  active code page, so non-ASCII here is the classic source of
rem  garbled output. All Chinese text lives in the .ps1 instead.
rem ====================================================================
chcp 65001 >nul
setlocal

set "PS1=%~dp0mem-diag.ps1"
if not exist "%PS1%" (
    echo [x] cannot find: %PS1%
    echo     mem-diag.ps1 must sit next to this .cmd
    pause
    exit /b 1
)

powershell -NoProfile -ExecutionPolicy Bypass -File "%PS1%" %*
set "RC=%ERRORLEVEL%"

rem --------------------------------------------------------------------
rem If your machine blocks scripts via Group Policy, "-ExecutionPolicy
rem Bypass" is ignored and you will see:
rem     ... cannot be loaded because running scripts is disabled
rem Fallback (reads the script text instead of loading the file, so the
rem execution policy does not apply):
rem     powershell -NoProfile -Command "iex (Get-Content -Raw '%PS1%')"
rem --------------------------------------------------------------------

echo.
pause
exit /b %RC%
