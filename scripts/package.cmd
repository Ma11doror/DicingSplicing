@echo off
rem Builds DicingSplicing into dist\DicingSplicing-<version>, ready to run, and zips it.
rem Arguments go on to package.ps1: -FFmpegDir <folder>, -YtDlp <file>, -NoZip.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0package.ps1" %*
set result=%errorlevel%
if "%~1"=="" pause
exit /b %result%
