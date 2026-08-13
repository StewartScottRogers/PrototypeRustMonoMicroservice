---
name: messaging-and-eventing
description: >
  The asynchronous half of this system — NATS/JetStream messaging vs eventing,
  the transactional outbox, idempotent consumers, retry and dead-lettering, and
  distributed tracing across the broker. Use when adding a message or event
  type, adding a consumer, debugging a message that was not delivered or was
  delivered twice, or when a trace stops at a service boundary.
---

# Messaging and eventing

```
you --POST /order--> gateway --order + message in ONE transaction--> postgres
                         |
                 outbox relay publishes
                         v
                [ORDER_COMMANDS stream]
                         |
           one shared consumer, two workers        <- MESSAGING: split
                         v
                     worker x2
                         |
                 publishes an event
                         v
                 [ORDER_EVENTS stream]
                    /            \                 <- EVENTING: copied
         notifier consumer     audit consumer
```

## The distinction, in one line

**Messaging** is a command — one consumer does the work. **Eventing** is a
fact — every subscriber gets a copy.

In JetStream both use the same streams and the same API. The *only* difference
is the durable consumer name:

| | Consumer name | Add a second process |
| --- | --- | --- |
| Messaging | shared (`order-worker`) | splits the load |
| Eventing | one each (`order-notifier`, `order-audit`) | adds a reaction |

If you cannot say which of the two a new subject is, it is a command.

## Non-negotiable rules

1. **Never publish inside the same operation that writes state.** Use the
   outbox: the row and the message go into one transaction, and a relay
   publishes afterwards. Publishing directly leaves a window where the state is
   committed and the message is lost, with nothing left to show it existed.

2. **Mark an outbox row published only after JetStream acknowledges it.**
   Marking first turns a broker blip into a lost message — the exact failure the
   outbox exists to prevent.

3. **Every consumer must be idempotent.** At-least-once delivery means
   duplicates are normal operation, not a bug. Either the work is naturally
   repeatable, or dedupe it — `IdempotencyGuard` for side effects, or
   `ON CONFLICT DO NOTHING` when the effect is a row.

4. **Release the idempotency claim when processing fails.** `guard.forget(id)`.
   Claiming the key up front and never releasing it means the retry is mistaken
   for a duplicate and the message is lost *because* you tried to be safe.

5. **Cap redelivery and dead-letter the remainder.** A message that can never
   succeed will otherwise be redelivered forever and block the queue behind it.
   `max_deliver` on the consumer must match `MAX_DELIVER` in the worker.

6. **Dedupe on the business key, not the message id.** `order_id`, not
   `envelope.id`. Message ids only catch broker redeliveries; business keys also
   catch a client submitting the same thing twice, which is the duplicate people
   actually hit.

7. **Publish with headers, always.** `Messaging::publish` injects the W3C
   `traceparent` so the consumer continues the trace. Bypassing it with a raw
   `jetstream.publish` produces an orphaned trace and no error.

## Adding a message or event type

1. Add the payload struct to `messaging-core/src/contract.rs`. Both ends of the
   wire come from that one definition, so a field change is a compile error in
   every producer and consumer rather than a decode failure in production.
2. Add the subject and any stream/consumer names to
   `messaging-core/src/subjects.rs`. Never write a subject string inline.
3. If it needs a new stream, add it to `Messaging::ensure_streams`.
4. Consumers: copy `notifier-service`. A *new reaction* gets a new consumer
   name; *more throughput* reuses the existing one.

## Tracing

Trace context rides in NATS headers. `trace::inject_current` writes it on
publish; `trace::span_for_message` reads it and parents the handling span. The
result is one trace covering the HTTP request, the broker hop and every
subscriber — which matters because eventing deliberately hides who caused what.

Confirm it end to end:

```
curl -s http://localhost:16686/api/services
curl -s "http://localhost:16686/api/traces?service=worker-service&limit=1"
```

## Debugging

- **A message is never delivered**: check the subject matches the stream's
  filter. `orders.command.>` will not capture `order.command.created`.
- **A message is delivered repeatedly**: something is not acking. An error
  before `message.ack()` means JetStream redelivers after `ack_wait`.
- **Two consumers split events instead of both getting them**: they share a
  durable name. That is messaging; give them separate names.
- **A trace stops at a service boundary**: the publisher bypassed
  `Messaging::publish`, or `init_tracing` ran without
  `OTEL_EXPORTER_OTLP_ENDPOINT` set.
- **Inspect the streams**: `http://localhost:8222/jsz?streams=1` on the NATS
  monitoring port shows message counts per stream, including the DLQ.

## Known gaps

1. **The outbox breaks the trace.** The HTTP request commits and returns; the
   relay publishes later on a different task, so it starts a new trace. Fixing
   it means persisting the `traceparent` in the outbox row and restoring it in
   the relay.
2. **The relay lives inside the gateway.** Fine at this size, but it means the
   gateway cannot be scaled without two relays competing — they would cope
   (`FOR UPDATE SKIP LOCKED`), but a separate process or change-data-capture is
   the real answer.
3. **Nothing drains the dead-letter stream.** Messages accumulate until a human
   looks. A replay tool is the obvious next piece.
4. **No schema versioning on the contracts.** A breaking change to
   `OrderCommand` would break consumers still reading older messages from the
   stream.
