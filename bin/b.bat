@echo off
setlocal
call "%~dp0cargo.bat" build %*
exit /b %ERRORLEVEL%
