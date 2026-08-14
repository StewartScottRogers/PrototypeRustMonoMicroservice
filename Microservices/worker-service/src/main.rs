//! The messaging half of the demonstration.
//!
//! Reads order commands from a JetStream queue, does the work exactly once,
//! and publishes an event saying it happened.
//!
//! Two copies of this service run in the compose stack. They share one durable
//! consumer name, so JetStream hands each command to exactly one of them — the
//! load splits rather than doubling. The `processed_by` field on the event is
//! how you see which one did it.
//!
//! It also demonstrates the two things a queue consumer must get right:
//!
//! - **Retry then dead-letter.** A command it cannot process is negatively
//!   acknowledged and redelivered. After [`MAX_DELIVER`] attempts it is moved
//!   to the dead-letter stream instead of blocking the queue forever.
//! - **Idempotency.** At-least-once delivery means the same order can arrive
//!   twice. Redis remembers which order ids are done, so the work happens once.

/// Contract tests: the shapes this service emits and accepts, checked against
/// the shared messaging-core types with no broker and no sibling running.
///
/// They live in src/ rather than tests/ because this is a binary-only crate:
/// Rust's tests/ directory can import a crate's lib target, and there isn't one.
/// See .claude/skills/microservice-agent-team/SKILL.md.
#[cfg(test)]
mod contract_tests;

use anyhow::{Context as _, Result};
use futures::StreamExt as _;
use messaging_core::contract::POISON_ITEM;
use messaging_core::{
    Envelope, IdempotencyGuard, Messaging, OrderCommand, OrderCompleted, subjects,
};
use service_core::{health, init_tracing, port_from_env, self_check};
use std::time::Duration;
use tracing::Instrument as _;

const SERVICE: &str = "worker-service";
const DEFAULT_PORT: u16 = 8080;

/// How many times JetStream will redeliver before this service gives up and
/// dead-letters the message. Must match the consumer's `max_deliver`.
const MAX_DELIVER: i64 = 3;

/// How long to wait before a redelivery. Real systems escalate this; a fixed
/// short delay keeps the demo watchable.
const RETRY_DELAY: Duration = Duration::from_secs(2);

#[tokio::main]
async fn main() -> Result<()> {
    let port = port_from_env("PORT", DEFAULT_PORT);

    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        std::process::exit(if self_check(port) { 0 } else { 1 });
    }

    init_tracing(SERVICE);
    service_core::init_metrics(SERVICE);

    // Docker sets HOSTNAME to the container id, which differs per replica. That
    // is what makes "which worker handled this" visible in the demo.
    let instance = std::env::var("HOSTNAME").unwrap_or_else(|_| "worker-unknown".to_owned());

    let nats_url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://nats:4222".to_owned());
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://redis:6379".to_owned());

    let messaging = Messaging::connect(&nats_url).await?;
    messaging.ensure_streams().await?;

    let guard = IdempotencyGuard::connect(&redis_url, "order").await?;

    tracing::info!(instance, "worker ready");

    // `tokio::select!` runs both futures on one task and returns as soon as
    // either finishes. Neither is expected to: the health server serves until
    // shutdown, and the consumer loops forever. If either *does* return, it is
    // an error worth stopping for.
    tokio::select! {
        result = health::serve(SERVICE, port) => {
            result.context("the health server stopped")?;
        }
        result = consume(messaging, guard, instance) => {
            result.context("the consumer loop stopped")?;
        }
    }

    Ok(())
}

/// The queue consumer loop.
///
/// `mut guard` because recording a processed id mutates the Redis connection.
async fn consume(
    messaging: Messaging,
    mut guard: IdempotencyGuard,
    instance: String,
) -> Result<()> {
    let mut messages = messaging
        .durable_consumer(
            subjects::STREAM_COMMANDS,
            // Shared name: this is what makes it messaging rather than eventing.
            subjects::CONSUMER_WORKER,
            subjects::ORDER_COMMAND_CREATED,
            MAX_DELIVER,
        )
        .await?;

    // `.next()` yields the next message, or None if the stream ends. `while
    // let Some(...)` is the idiomatic way to drain an async stream.
    while let Some(message) = messages.next().await {
        let message = message.context("could not read the next message")?;

        // Continue the trace the gateway started. Without this the worker's
        // work would appear as an unrelated trace, and the causal chain from
        // request to side effect would be lost - which is the whole reason
        // event-driven systems are hard to debug.
        let span = messaging_core::trace::span_for_message("worker", message.headers.as_ref());

        handle(&messaging, &mut guard, &instance, message)
            .instrument(span)
            .await?;
    }

    Ok(())
}

/// Handles one message. Split out from the loop so the whole of it, including
/// the publish, sits inside the trace span.
async fn handle(
    messaging: &Messaging,
    guard: &mut IdempotencyGuard,
    instance: &str,
    message: async_nats::jetstream::Message,
) -> Result<()> {
    {
        // How many times JetStream has tried to deliver this one, counting now.
        let delivered = message.info().map(|info| info.delivered).unwrap_or(1);

        let envelope: Envelope<OrderCommand> = match serde_json::from_slice(&message.payload) {
            Ok(envelope) => envelope,
            Err(error) => {
                // A message we cannot even decode will never succeed, so
                // retrying is pointless. Ack it so the queue moves on.
                tracing::error!(%error, "undecodable message, dropping");
                message.ack().await.ok();
                return Ok(());
            }
        };

        let order = &envelope.data;

        // A message written against a schema this build does not know must not
        // be guessed at. Dropping it loses data; processing it risks acting on
        // a misread payload. The dead-letter queue keeps it for a human.
        if !envelope.is_supported() {
            tracing::error!(
                order_id = %order.order_id,
                schema_version = envelope.schema_version,
                supported = messaging_core::envelope::SCHEMA_VERSION,
                "unsupported schema version, dead-lettering"
            );

            messaging
                .publish(
                    subjects::ORDER_DEAD_LETTER,
                    &Envelope::new("order.unsupported-schema", order.clone()),
                )
                .await?;

            message.ack().await.ok();
            return Ok(());
        }

        // Dedupe on the *order id*, not the message id: two separate publishes
        // of the same order are the duplicate worth catching, and each of those
        // carries its own message id.
        if !guard.first_time_seeing(order.order_id).await? {
            metrics::counter!(service_core::metrics::ORDERS_DUPLICATE).increment(1);
            tracing::info!(
                order_id = %order.order_id,
                "already processed, skipping (idempotency)"
            );
            message.ack().await.ok();
            return Ok(());
        }

        // Timed around the work itself, not the whole loop iteration, so the
        // measurement is of processing rather than of how long the message sat
        // waiting to be picked up.
        let started = std::time::Instant::now();
        let outcome = process(order, instance);
        metrics::histogram!(service_core::metrics::PROCESSING_SECONDS)
            .record(started.elapsed().as_secs_f64());

        match outcome {
            Ok(completed) => {
                messaging
                    .publish(
                        subjects::ORDER_EVENT_COMPLETED,
                        &Envelope::new("order.completed", completed),
                    )
                    .await?;

                metrics::counter!(service_core::metrics::ORDERS_PROCESSED).increment(1);
                message.ack().await.ok();
            }
            Err(error) => {
                // The attempt failed, so release the idempotency claim -
                // otherwise the redelivery would be mistaken for a duplicate
                // and the order would be silently lost.
                guard.forget(order.order_id).await?;

                if delivered >= MAX_DELIVER {
                    tracing::error!(
                        order_id = %order.order_id,
                        attempts = delivered,
                        %error,
                        "giving up, sending to the dead-letter queue"
                    );

                    messaging
                        .publish(
                            subjects::ORDER_DEAD_LETTER,
                            &Envelope::new("order.dead-letter", order.clone()),
                        )
                        .await?;

                    metrics::counter!(service_core::metrics::ORDERS_DEAD_LETTERED).increment(1);

                    // Ack the original: it is safely stored in the DLQ now, and
                    // leaving it unacked would block the queue.
                    message.ack().await.ok();
                } else {
                    tracing::warn!(
                        order_id = %order.order_id,
                        attempt = delivered,
                        %error,
                        "failed, will retry"
                    );

                    metrics::counter!(service_core::metrics::ORDERS_RETRIED).increment(1);

                    // Nak = "not acknowledged, try again", with a delay.
                    message
                        .ack_with(async_nats_ack_kind(RETRY_DELAY))
                        .await
                        .ok();
                }
            }
        }
    }

    Ok(())
}

/// Builds the "redeliver after this long" acknowledgement.
///
/// Split into its own function purely so the long type path does not obscure
/// the logic above.
fn async_nats_ack_kind(delay: Duration) -> async_nats::jetstream::AckKind {
    async_nats::jetstream::AckKind::Nak(Some(delay))
}

/// The actual work.
///
/// Fails on a specific item so retry and dead-lettering can be triggered on
/// demand instead of waiting for a real fault.
fn process(order: &OrderCommand, instance: &str) -> Result<OrderCompleted> {
    if order.item == POISON_ITEM {
        anyhow::bail!("refusing to process a poison item");
    }

    tracing::info!(
        order_id = %order.order_id,
        item = %order.item,
        quantity = order.quantity,
        worker = instance,
        "processed order"
    );

    Ok(OrderCompleted {
        order_id: order.order_id,
        item: order.item.clone(),
        quantity: order.quantity,
        processed_by: instance.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn order(item: &str) -> OrderCommand {
        OrderCommand {
            order_id: Uuid::new_v4(),
            item: item.to_owned(),
            quantity: 2,
        }
    }

    #[test]
    fn a_normal_order_is_processed_and_tagged_with_the_worker() {
        let command = order("widget");
        let completed = process(&command, "worker-7").unwrap();

        assert_eq!(completed.order_id, command.order_id);
        assert_eq!(completed.item, "widget");
        assert_eq!(completed.quantity, 2);
        assert_eq!(completed.processed_by, "worker-7");
    }

    #[test]
    fn a_poison_item_fails_so_retry_and_dlq_can_be_demonstrated() {
        assert!(process(&order(POISON_ITEM), "worker-7").is_err());
    }

    #[test]
    fn the_retry_ack_carries_the_configured_delay() {
        match async_nats_ack_kind(RETRY_DELAY) {
            async_nats::jetstream::AckKind::Nak(Some(delay)) => assert_eq!(delay, RETRY_DELAY),
            other => panic!("expected a delayed Nak, got {other:?}"),
        }
    }
}
