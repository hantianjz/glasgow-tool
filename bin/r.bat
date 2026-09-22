@echo off
setlocal
if "%~1"=="guart" goto valid_tool
if "%~1"=="c232uart" goto valid_tool
echo usage: bin\r.bat {guart^|c232uart} [arguments...] 1>&2
exit /b 2

:valid_tool
set "TOOL=%~1"
shift
set "TOOL_BINARY=%~dp0..\target\debug\%TOOL%.exe"
if not exist "%TOOL_BINARY%" (
  echo ERROR: %TOOL% is not built; run bin\b.bat first 1>&2
  exit /b 1
)
call "%TOOL_BINARY%" %*
exit /b %ERRORLEVEL%
