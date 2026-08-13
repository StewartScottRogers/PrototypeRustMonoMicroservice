//! Accepting orders, and getting them onto the broker without losing any.
//!
//! # The problem this module solves
//!
//! The obvious implementation is:
//!
//! ```text
//! 1. INSERT the order into Postgres, COMMIT
//! 2. publish the command to NATS
//! ```
//!
//! and it has a hole you cannot close by reordering. If the process dies
//! between 1 and 2, the order exists and nothing will ever process it. Swap the
//! steps and the opposite happens: a command is processed for an order that was
//! never stored.
//!
//! # The transactional outbox
//!
//! Write the order *and* the message into the same database transaction. Either
//! both land or neither does — that is what a transaction is for.
//!
//! ```text
//! BEGIN
//!   INSERT INTO orders  ...
//!   INSERT INTO outbox  ...   <- the message, waiting to be sent
//! COMMIT
//! ```
//!
//! A separate relay then reads unpublished rows and sends them. If it crashes
//! after publishing but before marking the row, it publishes again on restart —
//! at-least-once, which the consumers already handle by being idempotent.
//!
//! Losing a message is unrecoverable. Sending one twice is a solved problem.
//! The outbox trades the first for the second.

use anyhow::{Context as _, Result};
use axum::extract::State;
use axum::http::StatusCode;
use axum::{Json, Router, routing::post};
use messaging_core::{Envelope, Messaging, OrderCommand, subjects};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

/// How often the relay looks for unpublished rows.
const RELAY_INTERVAL: Duration = Duration::from_millis(250);

/// How many rows the relay sends per pass.
const RELAY_BATCH: i64 = 32;

/// What the handlers need beyond a single request.
#[derive(Clone)]
pub struct OrderState {
    pub pool: PgPool,
}

/// Request body for `POST /order`.
#[derive(Debug, Deserialize)]
pub struct PlaceOrder {
    pub item: String,
    pub quantity: u32,
    /// Optional. Supplying the same id twice is how the demo proves the worker
    /// is idempotent: two accepted orders, one lot of work done.
    ///
    /// `#[serde(default)]` makes the field optional in the JSON rather than
    /// required-but-nullable.
    #[serde(default)]
    pub order_id: Option<Uuid>,
}

/// Response body for `POST /order`.
#[derive(Debug, Serialize)]
pub struct OrderAccepted {
    pub order_id: Uuid,
    /// Always "accepted", never "completed". The work happens asynchronously,
    /// and saying otherwise would be a lie the caller might act on.
    pub status: &'static str,
}

/// Routes owned by this module.
pub fn routes(state: OrderState) -> Router {
    Router::new()
        .route("/order", post(place_order))
        .with_state(state)
}

/// Handles `POST /order`.
///
/// Returns `202 Accepted`, not `200 OK`: the order is durably recorded, but
/// nothing has processed it yet.
async fn place_order(
    State(state): State<OrderState>,
    Json(request): Json<PlaceOrder>,
) -> Result<(StatusCode, Json<OrderAccepted>), (StatusCode, String)> {
    // `unwrap_or_else` builds the fallback only when it is needed.
    let order_id = request.order_id.unwrap_or_else(Uuid::new_v4);

    accept(&state.pool, order_id, &request)
        .await
        .map_err(|error| {
            tracing::error!(%error, "could not accept the order");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not accept the order".to_owned(),
            )
        })?;

    Ok((
        StatusCode::ACCEPTED,
        Json(OrderAccepted {
            order_id,
            status: "accepted",
        }),
    ))
}

/// Writes the order and its outgoing message in one transaction.
///
/// `#[tracing::instrument]` wraps the whole call in a span automatically, and
/// records the listed fields on it. `skip(pool, request)` keeps the connection
/// pool and the raw body out of the trace; the fields worth seeing are named
/// explicitly instead.
#[tracing::instrument(skip(pool, request), fields(item = %request.item, quantity = request.quantity))]
async fn accept(pool: &PgPool, order_id: Uuid, request: &PlaceOrder) -> Result<()> {
    let command = OrderCommand {
        order_id,
        item: request.item.clone(),
        quantity: request.quantity,
    };
    let envelope = Envelope::new("order.created", command);
    let payload = serde_json::to_value(&envelope).context("could not encode the command")?;

    // `begin` opens a transaction. Dropping it without `commit` rolls back, so
    // an early `?` return cannot leave a half-written order behind.
    let mut transaction = pool
        .begin()
        .await
        .context("could not begin a transaction")?;

    // Re-posting the same order id is not an error - it is how idempotency is
    // demonstrated. The order row already exists, so leave it alone.
    sqlx::query(
        "INSERT INTO orders (order_id, item, quantity)
         VALUES ($1, $2, $3)
         ON CONFLICT (order_id) DO NOTHING",
    )
    .bind(order_id)
    .bind(&request.item)
    .bind(i32::try_from(request.quantity).unwrap_or(i32::MAX))
    .execute(&mut *transaction)
    .await
    .context("could not insert the order")?;

    // The message is queued here, in the same transaction, rather than being
    // published now. This is the whole point of the pattern.
    sqlx::query("INSERT INTO outbox (message_id, subject, payload) VALUES ($1, $2, $3)")
        .bind(envelope.id)
        .bind(subjects::ORDER_COMMAND_CREATED)
        .bind(&payload)
        .execute(&mut *transaction)
        .await
        .context("could not insert the outbox row")?;

    transaction
        .commit()
        .await
        .context("could not commit the order")?;

    tracing::info!(%order_id, item = %request.item, "order accepted and queued in the outbox");
    Ok(())
}

/// Publishes outbox rows to NATS, forever.
///
/// Runs as a background task inside the gateway. In a larger system this would
/// be its own process, or change-data-capture reading the write-ahead log; the
/// pattern is identical either way.
pub async fn relay(pool: PgPool, messaging: Messaging) -> Result<()> {
    tracing::info!("outbox relay started");

    loop {
        match publish_batch(&pool, &messaging).await {
            Ok(0) => tokio::time::sleep(RELAY_INTERVAL).await,
            // Rows were sent, so look again immediately - a burst should drain
            // at full speed rather than one batch per tick.
            Ok(sent) => tracing::debug!(sent, "relayed outbox rows"),
            Err(error) => {
                // Never exit the loop. The broker being briefly unavailable is
                // ordinary, and the rows are safe in Postgres until it returns.
                tracing::error!(%error, "outbox relay failed, retrying");
                tokio::time::sleep(RELAY_INTERVAL).await;
            }
        }
    }
}

/// One pass of the relay. Returns how many rows it published.
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
    .bind(RELAY_BATCH)
    .fetch_all(pool)
    .await
    .context("could not read the outbox")?;

    for (message_id, payload) in &rows {
        // One span per row, so each published message is its own trace rather
        // than a whole batch sharing one.
        let span = tracing::info_span!("outbox.publish", %message_id);
        let _guard = span.enter();

        let envelope: Envelope<OrderCommand> =
            serde_json::from_value(payload.clone()).context("an outbox row would not decode")?;

        messaging
            .publish(subjects::ORDER_COMMAND_CREATED, &envelope)
            .await?;

        // Marked only after JetStream confirms it stored the message. Marking
        // first would turn a broker failure into a lost message - the exact
        // thing the outbox exists to prevent.
        sqlx::query("UPDATE outbox SET published_at = now() WHERE message_id = $1")
            .bind(message_id)
            .execute(pool)
            .await
            .context("could not mark the outbox row published")?;
    }

    Ok(rows.len())
}
