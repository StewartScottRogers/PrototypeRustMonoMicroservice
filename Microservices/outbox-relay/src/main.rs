//! The bridge between a database transaction and the broker.
//!
//! The gateway writes an order and its outgoing message into Postgres in one
//! transaction, then returns. This service picks those rows up and publishes
//! them.
//!
//! # Why this is its own process
//!
//! It began as a background task inside the gateway, which worked but coupled
//! two unrelated things: running a second gateway for HTTP throughput would
//! have started a second relay whether you wanted one or not. Separated, each
//! scales for its own reason.
//!
//! Running more than one copy is safe either way — `FOR UPDATE SKIP LOCKED`
//! means each instance claims a different set of rows rather than colliding.
//!
//! # At-least-once, on purpose
//!
//! A row is marked published only after JetStream confirms it stored the
//! message. If this process dies in between, the next pass publishes it again.
//! That is deliberate: losing a message is unrecoverable, sending one twice is
//! a solved problem, and the consumers are idempotent.

use anyhow::{Context as _, Result};
use messaging_core::{Envelope, Messaging, OrderCommand, subjects};
use service_core::{health, init_tracing, port_from_env, self_check};
use sqlx::PgPool;
use std::time::Duration;
use tracing::Instrument as _;
use uuid::Uuid;

const SERVICE: &str = "outbox-relay";
const DEFAULT_PORT: u16 = 8080;

/// How long to wait when there was nothing to send.
const IDLE_INTERVAL: Duration = Duration::from_millis(250);

/// How many rows to publish per pass.
const BATCH: i64 = 32;

#[tokio::main]
async fn main() -> Result<()> {
    let port = port_from_env("PORT", DEFAULT_PORT);

    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        std::process::exit(if self_check(port) { 0 } else { 1 });
    }

    init_tracing(SERVICE);
    service_core::init_metrics(SERVICE);

    let database_url = std::env::var("DATABASE_URL")
        .context("DATABASE_URL must be set - the outbox lives in Postgres")?;
    let nats_url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://nats:4222".to_owned());

    let pool = db_core::connect_and_migrate(&database_url).await?;
    let messaging = Messaging::connect(&nats_url).await?;
    messaging.ensure_streams().await?;

    tokio::select! {
        result = health::serve(SERVICE, port) => {
            result.context("the health server stopped")?;
        }
        result = relay(pool, messaging) => {
            result.context("the relay loop stopped")?;
        }
    }

    Ok(())
}

/// Publishes outbox rows, forever.
async fn relay(pool: PgPool, messaging: Messaging) -> Result<()> {
    tracing::info!("outbox relay started");

    loop {
        match publish_batch(&pool, &messaging).await {
            Ok(0) => tokio::time::sleep(IDLE_INTERVAL).await,
            // Rows went out, so look again immediately - a burst should drain
            // at full speed rather than one batch per tick.
            Ok(sent) => tracing::debug!(sent, "relayed outbox rows"),
            Err(error) => {
                // Never exit the loop. The broker being briefly unavailable is
                // ordinary, and the rows are safe in Postgres until it returns.
                tracing::error!(%error, "outbox relay failed, retrying");
                tokio::time::sleep(IDLE_INTERVAL).await;
            }
        }
    }
}

/// One pass. Returns how many rows it published.
async fn publish_batch(pool: &PgPool, messaging: &Messaging) -> Result<usize> {
    // FOR UPDATE SKIP LOCKED is what makes this safe to run in more than one
    // process: each locks a different set of rows instead of colliding.
    let rows: Vec<(Uuid, serde_json::Value)> = sqlx::query_as(
        "SELECT message_id, payload
           FROM outbox
          WHERE published_at IS NULL
          ORDER BY created_at
          LIMIT $1
            FOR UPDATE SKIP LOCKED",
    )
    .bind(BATCH)
    .fetch_all(pool)
    .await
    .context("could not read the outbox")?;

    for (message_id, payload) in &rows {
        let envelope: Envelope<OrderCommand> =
            serde_json::from_value(payload.clone()).context("an outbox row would not decode")?;

        // The envelope carries the trace context captured when the HTTP request
        // wrote the row, so the publish joins *that* trace rather than starting
        // a fresh one here. Without it the chain from request to side effect
        // breaks exactly at the point the outbox makes it durable.
        let span = messaging_core::trace::span_from_map(SERVICE, &envelope.trace);

        async {
            messaging
                .publish(subjects::ORDER_COMMAND_CREATED, &envelope)
                .await?;

            // Marked only after JetStream confirms it. Marking first would turn
            // a broker failure into a lost message.
            sqlx::query("UPDATE outbox SET published_at = now() WHERE message_id = $1")
                .bind(message_id)
                .execute(pool)
                .await
                .context("could not mark the outbox row published")?;

            metrics::counter!(service_core::metrics::OUTBOX_RELAYED).increment(1);
            tracing::info!(%message_id, "relayed");
            Ok::<(), anyhow::Error>(())
        }
        .instrument(span)
        .await?;
    }

    Ok(rows.len())
}
