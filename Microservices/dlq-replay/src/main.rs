//! Emptying the dead-letter queue.
//!
//! A message that failed every delivery attempt is parked in the `ORDER_DLQ`
//! stream rather than discarded. That is the right call — the alternative
//! loses data — but a parked message helps nobody until something moves it
//! back.
//!
//! This is that something. Fix whatever caused the failure, then run:
//!
//! ```text
//! DevReplay.cmd            move every dead letter back onto the queue
//! DevReplay.cmd --dry-run  list them without touching anything
//! ```
//!
//! # Why a one-shot tool and not a service
//!
//! Automatic replay is a trap. A message is in the dead-letter queue precisely
//! because processing it failed repeatedly; replaying it on a timer produces
//! an infinite loop that looks like progress. Draining the queue is a decision
//! a person makes after fixing something, so it is a command a person runs.
//!
//! The worker is idempotent, so replaying a message that did in fact succeed
//! is harmless.

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
use messaging_core::{Envelope, Messaging, OrderCommand, subjects};
use service_core::init_tracing;
use std::time::Duration;

const SERVICE: &str = "dlq-replay";

/// How long to wait for another dead letter before deciding the queue is
/// empty.
///
/// The stream never ends on its own, so "empty" has to mean "nothing arrived
/// for a while".
const DRAIN_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a dry run holds a message back before the broker may redeliver it.
///
/// Only has to outlast one run. Deleting and recreating the consumer at the
/// start of the next run clears it regardless.
const DRY_RUN_HOLD: Duration = Duration::from_secs(60);

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing(SERVICE);

    // `any` returns true if the closure matches any argument. A flag is enough
    // here; a tool with real options would use a parser.
    let dry_run = std::env::args().any(|arg| arg == "--dry-run");

    let nats_url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://nats:4222".to_owned());
    let messaging = Messaging::connect(&nats_url).await?;
    messaging.ensure_streams().await?;

    let replayed = drain(&messaging, dry_run).await?;

    if dry_run {
        tracing::info!(found = replayed, "dry run: nothing was moved");
    } else {
        tracing::info!(replayed, "dead letters returned to the queue");
    }

    Ok(())
}

/// Reads every dead letter and republishes it as a command.
async fn drain(messaging: &Messaging, dry_run: bool) -> Result<usize> {
    // Always start from a clean consumer. A one-shot tool has no state worth
    // resuming, and an existing consumer would keep whatever settings it was
    // first created with - which is how an early version of this tool made the
    // queue undrainable, by spending each message's single delivery attempt on
    // a dry run that never acknowledged it.
    messaging
        .delete_consumer(subjects::STREAM_DEAD_LETTER, subjects::CONSUMER_DLQ_REPLAY)
        .await?;

    let mut messages = messaging
        .durable_consumer(
            subjects::STREAM_DEAD_LETTER,
            subjects::CONSUMER_DLQ_REPLAY,
            subjects::ORDER_DEAD_LETTER,
            // Unlimited redelivery. Anything finite means a failed or
            // abandoned drain permanently strands messages that are already,
            // by definition, the ones nobody wants to lose.
            -1,
        )
        .await?;

    let mut count = 0usize;

    loop {
        // `tokio::time::timeout` gives up waiting after a while and returns an
        // error, which is how a stream that never ends becomes a finite loop.
        let next = tokio::time::timeout(DRAIN_TIMEOUT, messages.next()).await;

        let message = match next {
            Err(_elapsed) => break, // nothing left
            Ok(None) => break,      // the stream closed
            Ok(Some(message)) => message.context("could not read a dead letter")?,
        };

        match serde_json::from_slice::<Envelope<OrderCommand>>(&message.payload) {
            Ok(envelope) => {
                let order = &envelope.data;
                count += 1;

                if dry_run {
                    tracing::info!(
                        order_id = %order.order_id,
                        item = %order.item,
                        "would replay"
                    );
                    // Hand it back, but not immediately. A bare Nak redelivers
                    // at once, so the drain loop keeps meeting the same
                    // messages and never reaches its idle timeout - a dry run
                    // that never ends. The delay outlasts this run; the next
                    // run recreates the consumer anyway, so nothing is stuck.
                    message
                        .ack_with(async_nats::jetstream::AckKind::Nak(Some(DRY_RUN_HOLD)))
                        .await
                        .ok();
                    continue;
                }

                // A fresh envelope, so the replayed message gets its own id
                // and timestamp. Reusing the original would make the
                // redelivery indistinguishable from the failure in the logs.
                messaging
                    .publish(
                        subjects::ORDER_COMMAND_CREATED,
                        &Envelope::new("order.created", order.clone()),
                    )
                    .await?;

                tracing::info!(order_id = %order.order_id, item = %order.item, "replayed");
                message.ack().await.ok();
            }
            Err(error) => {
                // Leave it. A dead letter nobody can decode is exactly the
                // kind of thing that should stay put until a human looks at
                // it.
                tracing::error!(%error, "undecodable dead letter, leaving it in place");
            }
        }
    }

    Ok(count)
}
