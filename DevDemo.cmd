@echo off
rem ---------------------------------------------------------------------------
rem Run the whole demonstration and narrate it.
rem
rem Assumes the stack is already up. Run DevStart.cmd first.
rem
rem Five things get demonstrated, in order:
rem   1. the transactional outbox   - order and message committed together
rem   2. messaging                  - two workers split the load
rem   3. eventing                   - two subscribers each react to one event
rem   4. idempotency                - the same order twice, processed once
rem   5. retry and dead-lettering   - a poison order gives up gracefully
rem ---------------------------------------------------------------------------
setlocal enabledelayedexpansion
pushd "%~dp0"

docker compose ps --quiet >nul 2>&1
if errorlevel 1 (
    echo [DevDemo] Docker is not responding. Start Docker Desktop first.
    goto :fail
)

for /f %%c in ('docker compose ps --quiet 2^>nul') do goto :running
echo [DevDemo] The stack is not running. Start it with DevStart.cmd first.
goto :fail

:running
set "GATEWAY_PORT=8080"
for /f "tokens=2 delims=:" %%p in ('docker compose port gateway-service 8080 2^>nul') do set "GATEWAY_PORT=%%p"
set "GW=http://localhost:%GATEWAY_PORT%"

echo.
echo ===========================================================================
echo  1. TRANSACTIONAL OUTBOX - the order and its message commit together
echo ===========================================================================
echo.
echo   POST %GW%/order  {"item":"widget","quantity":2}
echo.
curl.exe -s -X POST %GW%/order -H "Content-Type: application/json" -d "{\"item\":\"widget\",\"quantity\":2}"
echo.
echo.
echo   202 Accepted, not 200 OK - the order is durably stored, but nothing has
echo   processed it yet. The row and the outgoing message went into Postgres in
echo   ONE transaction, so a crash here cannot lose the message.
echo.
echo   The outbox, a moment after the write:
echo.
docker compose exec -T postgres psql -U devuser -d devdb -c "SELECT subject, (published_at IS NOT NULL) AS published FROM outbox ORDER BY created_at DESC LIMIT 3;"

echo.
echo ===========================================================================
echo  2. MESSAGING - one command, one worker. Load splits, it does not double.
echo ===========================================================================
echo.
echo   Sending five orders. Two workers share ONE durable consumer name, so
echo   JetStream gives each command to exactly one of them.
echo.
for /l %%i in (1,1,5) do (
    curl.exe -s -o nul -X POST %GW%/order -H "Content-Type: application/json" -d "{\"item\":\"widget-%%i\",\"quantity\":1}"
)
ping -n 4 127.0.0.1 >nul
echo   Which worker handled what:
echo.
rem --since keeps the output to this run. Without it the demo replays every
rem order the stack has ever handled, which buries the point.
docker compose logs --no-log-prefix --since 15s worker-service 2>nul | findstr /c:"processed order"
echo.
echo   Two different worker ids above means the load split. That is messaging.

echo.
echo ===========================================================================
echo  3. EVENTING - one event, every subscriber reacts
echo ===========================================================================
echo.
echo   The worker published order.completed. It does not know who is listening.
echo   Two services are, each with its own durable consumer name, so each gets
echo   its own copy:
echo.
echo   notifier-service:
docker compose logs --no-log-prefix --since 20s notifier-service 2>nul | findstr /c:"reacted to order.completed"
echo.
echo   audit-service:
docker compose logs --no-log-prefix --since 20s audit-service 2>nul | findstr /c:"reacted to order.completed"
echo.
echo   Same event, two independent reactions. Adding a third subscriber would
echo   need no change to the worker at all. That is eventing.
echo.
echo   What the auditor wrote to Postgres:
echo.
docker compose exec -T postgres psql -U devuser -d devdb -c "SELECT item, quantity, processed_by FROM audit_log ORDER BY recorded_at DESC LIMIT 5;"

echo.
echo ===========================================================================
echo  4. IDEMPOTENCY - the same order twice, processed once
echo ===========================================================================
echo.
set "DUP=11111111-2222-3333-4444-555555555555"
echo   Posting order_id %DUP% twice.
echo.
curl.exe -s -o nul -X POST %GW%/order -H "Content-Type: application/json" -d "{\"order_id\":\"%DUP%\",\"item\":\"duplicate-me\",\"quantity\":1}"
ping -n 3 127.0.0.1 >nul
curl.exe -s -o nul -X POST %GW%/order -H "Content-Type: application/json" -d "{\"order_id\":\"%DUP%\",\"item\":\"duplicate-me\",\"quantity\":1}"
ping -n 4 127.0.0.1 >nul
echo   The worker's view:
echo.
docker compose logs --no-log-prefix --since 15s worker-service 2>nul | findstr /c:"%DUP%"
echo.
echo   Processed once, skipped once. At-least-once delivery means duplicates are
echo   normal, so the consumer - not the broker - has to cope. Redis remembers.

echo.
echo ===========================================================================
echo  5. RETRY AND DEAD-LETTERING - failing without blocking the queue
echo ===========================================================================
echo.
echo   Sending an order the worker refuses to process.
echo.
curl.exe -s -o nul -X POST %GW%/order -H "Content-Type: application/json" -d "{\"item\":\"poison\",\"quantity\":1}"
echo   Waiting for three delivery attempts...
ping -n 12 127.0.0.1 >nul
echo.
docker compose logs --no-log-prefix --since 20s worker-service 2>nul | findstr /c:"will retry" /c:"dead-letter"
echo.
echo   Retried, then moved aside. The queue keeps flowing rather than jamming on
echo   one bad message, and nothing is silently discarded.
echo.
echo   Messages parked in the dead-letter stream:
echo.
docker compose exec -T nats sh -c "wget -q -O - http://localhost:8222/jsz?streams=1 2>/dev/null" | findstr /i "ORDER_DLQ messages"

echo.
echo ===========================================================================
echo  6. ONE TRACE, END TO END
echo ===========================================================================
echo.
echo   Every hop above - the incoming request, the outbox relay, the queue,
echo   both subscribers - is one trace. Open Jaeger and pick "gateway-service":
echo.
echo       http://localhost:16686
echo.
echo   That is the payoff: eventing decouples services, which also makes the
echo   causal chain invisible. Tracing is what gives it back.
echo.

popd
endlocal
exit /b 0

:fail
popd
endlocal
exit /b 1
