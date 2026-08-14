@echo off
rem ---------------------------------------------------------------------------
rem Move dead letters back onto the command queue.
rem
rem   DevReplay.cmd             replay every dead letter
rem   DevReplay.cmd --dry-run   list them, change nothing
rem
rem Run this AFTER fixing whatever made the messages fail. A message reaches the
rem dead-letter queue because processing it failed three times; replaying before
rem the cause is fixed just fills the queue again.
rem
rem Replaying something that actually succeeded is harmless - the worker
rem deduplicates on the order id.
rem ---------------------------------------------------------------------------
setlocal
pushd "%~dp0"

docker compose version >nul 2>&1
if errorlevel 1 (
    echo [DevReplay] Docker is not responding. Start Docker Desktop first.
    goto :fail
)

for /f %%c in ('docker compose ps --quiet 2^>nul') do goto :running
echo [DevReplay] The stack is not running. Start it with DevStart.cmd first.
goto :fail

:running
echo [DevReplay] Draining the dead-letter queue...
echo.
rem --rm removes the container afterwards; this is a task, not a service.
rem %* forwards --dry-run through to the tool.
docker compose run --rm dlq-replay %*
if errorlevel 1 goto :fail

echo.
echo [DevReplay] Done. Watch them being reprocessed with: DevLogs.cmd worker-service

popd
endlocal
exit /b 0

:fail
popd
endlocal
exit /b 1
