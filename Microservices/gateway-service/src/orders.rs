//! Accepting orders without losing any.
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
//! `outbox-relay` then publishes those rows. That used to be a background task
//! in this service; it is a separate process now, so HTTP throughput and relay
//! throughput scale for their own reasons.
//!
//! Losing a message is unrecoverable. Sending one twice is a solved problem.
//! The outbox trades the first for the second.

use anyhow::{Context as _, Result};
use axum::extract::State;
use axum::http::StatusCode;
use axum::{Json, Router, routing::post};
use messaging_core::{Envelope, OrderCommand, subjects};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

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

    // Built inside this span on purpose: Envelope::new captures the active
    // trace context into the envelope, which is then stored as JSON in the
    // outbox row. That is how the trace survives until the relay picks it up.
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

    metrics::counter!(service_core::metrics::ORDERS_ACCEPTED).increment(1);
    tracing::info!(%order_id, item = %request.item, "order accepted and queued in the outbox");
    Ok(())
}
