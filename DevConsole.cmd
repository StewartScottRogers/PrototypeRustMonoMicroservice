@echo off
rem ---------------------------------------------------------------------------
rem Open the console: every graphical interface in one page.
rem
rem   DevConsole.cmd
rem
rem Starts the stack first if it is not already running, so this is the only
rem command you need from cold. If everything is already up it just opens the
rem browser, which takes about a second.
rem
rem The console frames the mimic panel, the walkthrough, Grafana, Jaeger and
rem Prometheus, with a live status strip across the top.
rem ---------------------------------------------------------------------------
setlocal enabledelayedexpansion
pushd "%~dp0"

docker compose version >nul 2>&1
if errorlevel 1 (
    echo [DevConsole] Docker is not responding.
    echo [DevConsole] Start Docker Desktop, wait for it to finish starting, then run this again.
    goto :fail
)

rem Is the console already answering? If so there is nothing to start.
curl.exe -s -o nul -m 2 http://localhost:%MIMIC_PORT%/healthz >nul 2>&1
set "RUNNING="
for /f %%c in ('docker compose ps --quiet mimic-service 2^>nul') do set "RUNNING=1"

if defined RUNNING (
    rem Present but possibly stopped. Anything not running gets started below.
    docker compose ps --format "{{.Status}}" 2>nul | findstr /i "Up" >nul
    if errorlevel 1 set "RUNNING="
)

if not defined RUNNING (
    echo [DevConsole] The stack is not running. Starting it now - this takes a
    echo              couple of minutes the first time, about 30 seconds after that.
    echo.
    call "%~dp0DevStart.cmd"
    if errorlevel 1 (
        echo [DevConsole] The stack did not come up. See the output above.
        goto :fail
    )
) else (
    echo [DevConsole] Stack already running.
)

rem Ask compose which host port was actually published rather than assuming
rem 8090 - a .env file or a taken port changes it.
set "MIMIC_PORT=8090"
for /f "tokens=2 delims=:" %%p in ('docker compose port mimic-service 8080 2^>nul') do set "MIMIC_PORT=%%p"

echo.
echo [DevConsole] Opening http://localhost:%MIMIC_PORT%/
echo.
echo   Mimic panel   live topology, lamps and gauges
echo   Walkthrough   the written explanation
echo   Grafana       dashboards and history
echo   Traces        one request across every service
echo   Prometheus    raw queries
echo.
echo   Press 1-5 in the console to switch tabs.
echo.

rem `start ""` with an empty title, or cmd treats the web address as the window title.
start "" "http://localhost:%MIMIC_PORT%/"

popd
endlocal
exit /b 0

:fail
popd
endlocal
exit /b 1
