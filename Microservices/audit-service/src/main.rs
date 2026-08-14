//! The eventing half of the demonstration — subscriber two of two.
//!
//! Subscribes to the same `orders.event.completed` as `notifier-service` and
//! does something completely different with it: writes an audit row to
//! Postgres.
//!
//! Two subscribers, one event, two independent reactions, and a publisher that
//! knows about neither. Run the demo and watch both react to the same order.

use anyhow::{Context as _, Result};
use futures::StreamExt as _;
use messaging_core::{Envelope, Messaging, OrderCompleted, subjects};
use service_core::{health, init_tracing, port_from_env, self_check};
use sqlx::PgPool;
use tracing::Instrument as _;

const SERVICE: &str = "audit-service";
const DEFAULT_PORT: u16 = 8080;
const MAX_DELIVER: i64 = 1;

#[tokio::main]
async fn main() -> Result<()> {
    let port = port_from_env("PORT", DEFAULT_PORT);

    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        std::process::exit(if self_check(port) { 0 } else { 1 });
    }

    init_tracing(SERVICE);

    let nats_url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://nats:4222".to_owned());
    let database_url = std::env::var("DATABASE_URL")
        .context("DATABASE_URL must be set - this service records to Postgres")?;

    // The schema lives in db-core, shared with the gateway - see that crate for
    // why it is not duplicated per service.
    let pool = db_core::connect_and_migrate(&database_url).await?;

    let messaging = Messaging::connect(&nats_url).await?;
    messaging.ensure_streams().await?;

    tokio::select! {
        result = health::serve(SERVICE, port) => {
            result.context("the health server stopped")?;
        }
        result = consume(messaging, pool) => {
            result.context("the subscriber loop stopped")?;
        }
    }

    Ok(())
}

async fn consume(messaging: Messaging, pool: PgPool) -> Result<()> {
    let mut messages = messaging
        .durable_consumer(
            subjects::STREAM_EVENTS,
            // Different from the notifier's name, which is why both see every
            // event rather than sharing them out.
            subjects::CONSUMER_AUDIT,
            subjects::ORDER_EVENT_COMPLETED,
            MAX_DELIVER,
        )
        .await?;

    while let Some(message) = messages.next().await {
        let message = message.context("could not read the next event")?;

        let span = messaging_core::trace::span_for_message("audit", message.headers.as_ref());

        async {
            match serde_json::from_slice::<Envelope<OrderCompleted>>(&message.payload) {
                Ok(envelope) if !envelope.is_supported() => {
                    tracing::error!(
                        schema_version = envelope.schema_version,
                        supported = messaging_core::envelope::SCHEMA_VERSION,
                        "unsupported schema version, skipping this event"
                    );
                }
                Ok(envelope) => {
                    if let Err(error) = record(&pool, &envelope).await {
                        tracing::error!(%error, "could not write the audit row");
                    } else {
                        let event = &envelope.data;
                        tracing::info!(
                            order_id = %event.order_id,
                            processed_by = %event.processed_by,
                            reaction = "audit row written",
                            "reacted to order.completed"
                        );
                    }
                }
                Err(error) => {
                    tracing::error!(%error, "undecodable event, skipping");
                }
            }

            message.ack().await.ok();
        }
        .instrument(span)
        .await;
    }

    Ok(())
}

/// Writes one audit row.
///
/// `ON CONFLICT DO NOTHING` keyed on the message id makes this safe to run
/// twice: at-least-once delivery means a redelivery is normal, and the database
/// itself is the cleanest place to make the second write a no-op.
///
/// Note the `$1`-style placeholders and `.bind(...)`. Values never go into the
/// SQL string, so a hostile item name cannot become SQL.
async fn record(pool: &PgPool, envelope: &Envelope<OrderCompleted>) -> Result<()> {
    sqlx::query(
        "INSERT INTO audit_log (message_id, order_id, item, quantity, processed_by, occurred_at)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (message_id) DO NOTHING",
    )
    .bind(envelope.id)
    .bind(envelope.data.order_id)
    .bind(&envelope.data.item)
    .bind(i32::try_from(envelope.data.quantity).unwrap_or(i32::MAX))
    .bind(&envelope.data.processed_by)
    .bind(envelope.occurred_at)
    .execute(pool)
    .await
    .context("the audit insert failed")?;

    Ok(())
}
