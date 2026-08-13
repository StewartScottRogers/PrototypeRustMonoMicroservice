@echo off
rem ---------------------------------------------------------------------------
rem Stop the stack, keeping the containers.
rem
rem Nothing is deleted: DevStart.cmd brings the same containers back, and any
rem data in the Postgres and Redis volumes is untouched.
rem ---------------------------------------------------------------------------
setlocal
pushd "%~dp0"

docker compose version >nul 2>&1
if errorlevel 1 (
    echo [DevStop] Docker is not responding, so there is nothing to stop.
    goto :fail
)

echo [DevStop] Stopping containers...
docker compose stop
if errorlevel 1 goto :fail

echo [DevStop] Stopped. Start again with DevStart.cmd.

popd
endlocal
exit /b 0

:fail
popd
endlocal
exit /b 1
