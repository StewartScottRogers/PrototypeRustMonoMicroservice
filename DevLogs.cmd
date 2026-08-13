@echo off
rem ---------------------------------------------------------------------------
rem Follow the logs. Ctrl-C stops following; it does not stop the containers.
rem
rem   DevLogs.cmd                   every service
rem   DevLogs.cmd gateway-service   just one
rem ---------------------------------------------------------------------------
setlocal
pushd "%~dp0"

docker compose version >nul 2>&1
if errorlevel 1 (
    echo [DevLogs] Docker is not responding.
    goto :fail
)

rem %* forwards any service names straight through to compose.
docker compose logs --follow --tail=100 %*

popd
endlocal
exit /b 0

:fail
popd
endlocal
exit /b 1
