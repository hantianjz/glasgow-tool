@echo off
setlocal
call "%~dp0uv.bat" run --project "%~dp0..\gateware" --frozen python "%~dp0..\gateware\build_uart.py" --output "%~dp0..\target\glasgow-uart"
if errorlevel 1 exit /b %ERRORLEVEL%
call "%~dp0cargo.bat" build %*
exit /b %ERRORLEVEL%
