#!/usr/bin/env bash
#
# End-to-end check of the compose stack. This is the Linux counterpart of
# DevDemo.cmd: the .cmd file narrates for a human, this one asserts for CI.
#
#   .github/scripts/smoke-test.sh
#
# Assumes `docker compose up -d --wait` has already run. Exits non-zero on the
# first failed assertion.

set -euo pipefail

GATEWAY="${GATEWAY:-http://localhost:8080}"
JAEGER="${JAEGER:-http://localhost:16686}"

pass() { printf '  ok    %s\n' "$1"; }
fail() {
    printf '  FAIL  %s\n' "$1" >&2
    exit 1
}

# Nothing here is instant: work crosses a broker, so every assertion polls
# rather than sleeping a fixed amount and hoping.
wait_for() {
    local description="$1" attempts="$2"
    shift 2
    for _ in $(seq "$attempts"); do
        if "$@" >/dev/null 2>&1; then
            pass "$description"
            return 0
        fi
        sleep 1
    done
    fail "$description"
}

worker_logs_contain() { docker compose logs --no-log-prefix worker-service 2>/dev/null | grep -q "$1"; }
notifier_logs_contain() { docker compose logs --no-log-prefix notifier-service 2>/dev/null | grep -q "$1"; }
audit_logs_contain() { docker compose logs --no-log-prefix audit-service 2>/dev/null | grep -q "$1"; }

psql_count() {
    docker compose exec -T postgres \
        psql -U devuser -d devdb -tAc "$1" 2>/dev/null | tr -d '[:space:]'
}

echo "1. the outbox accepts an order"
order_id="$(curl -sf -X POST "$GATEWAY/order" \
    -H 'Content-Type: application/json' \
    -d '{"item":"widget","quantity":2}' | grep -o '"order_id":"[^"]*"' | cut -d'"' -f4)"
[ -n "$order_id" ] || fail "POST /order returned no order_id"
pass "accepted $order_id"

echo "2. messaging: a worker picks the command up"
wait_for "a worker processed it" 30 worker_logs_contain "$order_id"

echo "3. eventing: both subscribers react to the same event"
wait_for "notifier reacted" 30 notifier_logs_contain "$order_id"
wait_for "audit reacted" 30 audit_logs_contain "$order_id"

echo "4. the audit row reached Postgres"
wait_for "audit_log has the row" 30 \
    bash -c "[ \"\$(docker compose exec -T postgres psql -U devuser -d devdb -tAc \"SELECT count(*) FROM audit_log WHERE order_id = '$order_id'\" 2>/dev/null | tr -d '[:space:]')\" = 1 ]"

echo "5. the outbox row is marked published"
published="$(psql_count "SELECT count(*) FROM outbox WHERE published_at IS NOT NULL")"
[ "${published:-0}" -ge 1 ] || fail "no outbox row was marked published"
pass "$published row(s) published"

echo "6. idempotency: the same order twice is processed once"
dup="11111111-2222-3333-4444-555555555555"
for _ in 1 2; do
    curl -sf -o /dev/null -X POST "$GATEWAY/order" \
        -H 'Content-Type: application/json' \
        -d "{\"order_id\":\"$dup\",\"item\":\"duplicate-me\",\"quantity\":1}"
    sleep 2
done
wait_for "the duplicate was skipped" 30 worker_logs_contain "already processed, skipping"

echo "7. retry and dead-lettering"
curl -sf -o /dev/null -X POST "$GATEWAY/order" \
    -H 'Content-Type: application/json' \
    -d '{"item":"poison","quantity":1}'
wait_for "it was retried" 40 worker_logs_contain "failed, will retry"
wait_for "it was dead-lettered" 40 worker_logs_contain "dead-letter"

echo "8. every service reported traces"
for service in gateway-service worker-service notifier-service audit-service; do
    wait_for "$service is in Jaeger" 30 \
        bash -c "curl -sf '$JAEGER/api/services' | grep -q '$service'"
done

echo
echo "smoke test passed"
