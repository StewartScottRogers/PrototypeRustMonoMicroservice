-- The whole schema for the demonstration stack.

-- Orders the gateway has accepted.
CREATE TABLE IF NOT EXISTS orders (
    order_id   UUID PRIMARY KEY,
    item       TEXT        NOT NULL,
    quantity   INTEGER     NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The transactional outbox.
--
-- The gateway writes the order and the message it wants to send in ONE
-- transaction. Publishing straight to the broker instead would leave a window
-- in which the order is committed but the process dies before publishing, and
-- the message is lost with nothing to show it ever existed.
--
-- A relay then reads unpublished rows and sends them. Worst case it publishes
-- twice, which is exactly the at-least-once delivery the consumers already
-- handle. Losing a message is unrecoverable; sending one twice is not.
CREATE TABLE IF NOT EXISTS outbox (
    message_id   UUID PRIMARY KEY,
    subject      TEXT        NOT NULL,
    payload      JSONB       NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    published_at TIMESTAMPTZ
);

-- The relay only ever looks for unpublished rows, so index just those. A
-- partial index stays small no matter how much history accumulates.
CREATE INDEX IF NOT EXISTS outbox_unpublished_idx
    ON outbox (created_at)
    WHERE published_at IS NULL;

-- Audit trail written by audit-service in reaction to order.completed events.
--
-- message_id is the primary key rather than a serial, which makes the insert
-- naturally idempotent: a redelivered event carries the same id and conflicts.
CREATE TABLE IF NOT EXISTS audit_log (
    message_id   UUID PRIMARY KEY,
    order_id     UUID        NOT NULL,
    item         TEXT        NOT NULL,
    quantity     INTEGER     NOT NULL,
    processed_by TEXT        NOT NULL,
    occurred_at  TIMESTAMPTZ NOT NULL,
    recorded_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS audit_log_order_id_idx ON audit_log (order_id);
