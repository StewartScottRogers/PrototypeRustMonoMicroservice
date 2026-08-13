@echo off
rem ---------------------------------------------------------------------------
rem Build and start the local development stack, then wait until every container
rem reports healthy and print the URLs.
rem
rem Safe to run repeatedly: compose only rebuilds and restarts what changed.
rem ---------------------------------------------------------------------------
setlocal enabledelayedexpansion
pushd "%~dp0"

docker compose version >nul 2>&1
if errorlevel 1 (
    echo [DevStart] Docker is not responding.
    echo [DevStart] Start Docker Desktop, wait for it to finish starting, then run this again.
    goto :fail
)

echo [DevStart] Building images and starting containers...
docker compose up --detach --build
if errorlevel 1 (
    echo [DevStart] Compose failed to start. The output above says why.
    goto :fail
)

echo.
echo [DevStart] Waiting for health checks, up to 120 seconds...

rem A container reports "starting" until its healthcheck first succeeds. Poll
rem until none of them do, then decide whether that ended well or badly.
set "READY="
for /l %%i in (1,1,60) do (
    if not defined READY (
        docker compose ps --format "{{.Health}}" | findstr /i "starting" >nul
        if errorlevel 1 (
            set "READY=1"
        ) else (
            rem `ping` rather than `timeout`, which refuses to run when stdin is
            rem redirected - as it is from CI or another script. Three pings to
            rem localhost is about two seconds.
            ping -n 3 127.0.0.1 >nul
        )
    )
)

docker compose ps --format "{{.Health}}" | findstr /i "unhealthy" >nul
if not errorlevel 1 (
    echo.
    echo [DevStart] At least one container is unhealthy:
    docker compose ps
    echo.
    echo [DevStart] Look at the logs with: DevLogs.cmd
    goto :fail
)

if not defined READY (
    echo.
    echo [DevStart] Timed out waiting for containers to become healthy:
    docker compose ps
    echo.
    echo [DevStart] Look at the logs with: DevLogs.cmd
    goto :fail
)

rem Ask compose which host ports it actually published, rather than assuming the
rem defaults - a .env file or an already-taken port can change them.
set "GATEWAY_PORT=8080"
set "ECHO_PORT=8081"
for /f "tokens=2 delims=:" %%p in ('docker compose port gateway-service 8080 2^>nul') do set "GATEWAY_PORT=%%p"
for /f "tokens=2 delims=:" %%p in ('docker compose port echo-service 8080 2^>nul') do set "ECHO_PORT=%%p"

echo.
echo [DevStart] The stack is up.
echo.
echo   gateway-service   http://localhost:%GATEWAY_PORT%/healthz
echo   echo-service      http://localhost:%ECHO_PORT%/healthz
echo   postgres          localhost:5432   user=devuser db=devdb
echo   redis             localhost:6379
echo.
echo   End-to-end test - gateway calls echo-service internally:
echo.
echo     curl -X POST http://localhost:%GATEWAY_PORT%/relay -H "Content-Type: application/json" -d "{\"message\":\"hello\"}"
echo.
echo   Expected: {"echo":"hello","via":"gateway-service"}
echo.

popd
endlocal
exit /b 0

:fail
popd
endlocal
exit /b 1
