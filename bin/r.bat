@echo off
setlocal
set "GLASGOW_TOOL=%~dp0..\target\debug\glasgow-tool.exe"
if not exist "%GLASGOW_TOOL%" (
  echo ERROR: glasgow-tool is not built; run bin\b.bat first 1>&2
  exit /b 1
)
call "%GLASGOW_TOOL%" %*
exit /b %ERRORLEVEL%
