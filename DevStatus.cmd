@echo off
rem ---------------------------------------------------------------------------
rem Show what exists, what is running, and whether it is healthy.
rem ---------------------------------------------------------------------------
setlocal
pushd "%~dp0"

docker compose version >nul 2>&1
if errorlevel 1 (
    echo [DevStatus] Docker is not responding. Start Docker Desktop first.
    goto :fail
)

echo [DevStatus] Containers:
echo.
rem --all so stopped containers still appear. Without it, DevStop.cmd makes the
rem stack look deleted when it has only been paused.
docker compose ps --all
echo.

rem --quiet prints one container id per line, so no output means none exist.
for /f %%c in ('docker compose ps --all --quiet 2^>nul') do goto :exists
echo [DevStatus] No containers exist. Create them with DevStart.cmd.
goto :done

:exists
for /f %%c in ('docker compose ps --quiet 2^>nul') do goto :running
echo [DevStatus] Containers exist but none are running. Start them with DevStart.cmd.
goto :volumes

:running
echo [DevStatus] Stack is running.

:volumes
echo.
echo [DevStatus] Volumes:
echo.
docker volume ls --filter "label=com.docker.compose.project=prototype-rust-microservices"

:done
popd
endlocal
exit /b 0

:fail
popd
endlocal
exit /b 1
