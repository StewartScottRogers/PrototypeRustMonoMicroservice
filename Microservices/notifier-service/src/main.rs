//! The eventing half of the demonstration — subscriber one of two.
//!
//! Subscribes to `orders.event.completed` and pretends to send a notification.
//!
//! # What makes this eventing rather than messaging
//!
//! This service and `audit-service` both consume the *same* subject, and both
//! see *every* event. That is because they use different durable consumer
//! names, so each has its own independent position in the stream.
//!
//! Compare `worker-service`: two copies of it share one consumer name, so each
//! command goes to only one of them.
//!
//! Nothing in the publisher knows either of these services exists. Adding a
//! third subscriber needs no change to `worker-service` at all — that
//! decoupling is the entire point of publishing events.

/// Contract tests: the shapes this service emits and accepts, checked against
/// the shared messaging-core types with no broker and no sibling running.
///
/// They live in src/ rather than tests/ because this is a binary-only crate:
/// Rust's tests/ directory can import a crate's lib target, and there isn't
/// one. See .claude/skills/microservice-agent-team/SKILL.md.
#[cfg(test)]
mod contract_tests;

use anyhow::{Context as _, Result};
use futures::StreamExt as _;
use messaging_core::{Envelope, Messaging, OrderCompleted, subjects};
use service_core::{health, init_tracing, port_from_env, self_check};
use tracing::Instrument as _;

const SERVICE: &str = "notifier-service";
const DEFAULT_PORT: u16 = 8080;

/// Events are facts about the past, so there is nothing to retry: this service
/// either reacts or it does not. One delivery attempt is enough.
const MAX_DELIVER: i64 = 1;

#[tokio::main]
async fn main() -> Result<()> {
    let port = port_from_env("PORT", DEFAULT_PORT);

    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        std::process::exit(if self_check(port) { 0 } else { 1 });
    }

    init_tracing(SERVICE);
    service_core::init_metrics(SERVICE);

    let nats_url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://nats:4222".to_owned());
    let messaging = Messaging::connect(&nats_url).await?;
    messaging.ensure_streams().await?;

    tokio::select! {
        result = health::serve(SERVICE, port) => {
            result.context("the health server stopped")?;
        }
        result = consume(messaging) => {
            result.context("the subscriber loop stopped")?;
        }
    }

    Ok(())
}

async fn consume(messaging: Messaging) -> Result<()> {
    let mut messages = messaging
        .durable_consumer(
            subjects::STREAM_EVENTS,
            // A name nobody else uses. That is the whole difference.
            subjects::CONSUMER_NOTIFIER,
            subjects::ORDER_EVENT_COMPLETED,
            MAX_DELIVER,
        )
        .await?;

    while let Some(message) = messages.next().await {
        let message = message.context("could not read the next event")?;

        // Continues the trace that began with the Hypertext Transfer Protocol
        // request, so this reaction shows up under the order that caused it.
        let span = messaging_core::trace::span_for_message("notifier", message.headers.as_ref());

        async {
            match serde_json::from_slice::<Envelope<OrderCompleted>>(&message.payload) {
                Ok(envelope) if !envelope.is_supported() => {
                    // Subscribers have no dead-letter queue of their own: an
                    // event they cannot read is a fact they miss, not work
                    // left undone. Log it loudly and move on.
                    tracing::error!(
                        schema_version = envelope.schema_version,
                        supported = messaging_core::envelope::SCHEMA_VERSION,
                        "unsupported schema version, skipping this event"
                    );
                }
                Ok(envelope) => {
                    // Labelled by service, so one metric covers every
                    // subscriber and an imbalance between them is visible.
                    metrics::counter!(service_core::metrics::EVENTS_HANDLED, "service" => SERVICE)
                        .increment(1);

                    let event = &envelope.data;
                    tracing::info!(
                        order_id = %event.order_id,
                        item = %event.item,
                        quantity = event.quantity,
                        processed_by = %event.processed_by,
                        reaction = "notification sent",
                        "reacted to order.completed"
                    );
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
