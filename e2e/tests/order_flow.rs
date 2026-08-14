//! End-to-end: one order through every service.
//!
//! ```text
//! POST /order → gateway → outbox (Postgres, one txn)
//!                            ↓ relay
//!                        NATS ORDER_COMMANDS → worker
//!                            ↓ publishes
//!                        NATS ORDER_EVENTS → notifier
//!                                          → audit → Postgres
//! ```
//!
//! # Why these are `#[ignore]`
//!
//! Every test here needs a composed stack. Left un-ignored, `cargo test` would
//! fail on any machine that has not run `DevStart.cmd` — and a suite that is
//! red by default is a suite people stop reading.
//!
//! ```text
//! DevStart.cmd
//! cargo test -p e2e -- --ignored --test-threads=1
//! ```
//!
//! `--test-threads=1` because these share one stack and one database. Run in
//! parallel they would count each other's orders.
//!
//! # Why the assertions poll
//!
//! Nothing past the gateway is synchronous. The POST returns `202 Accepted`
//! while the work is still queued, so an immediate assertion would be testing
//! the speed of the machine. Every check goes through [`e2e::wait_until`].

use anyhow::{Context as _, Result};
use e2e::{Harness, wait_until};
use messaging_core::{Envelope, OrderCommand};

/// Counts audit rows for one order.
///
/// The audit service writes exactly one row per completed order, keyed by the
/// message id, so this is the cleanest end-of-chain evidence that the whole
/// path ran: gateway accepted, relay published, worker processed, audit
/// reacted.
async fn audit_rows(pool: &sqlx::PgPool, order_id: uuid::Uuid) -> Result<i64> {
    // `query_scalar` returns the single column of the single row. The `$1`
    // placeholder keeps the value out of the SQL string, so an id can never be
    // interpreted as SQL.
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE order_id = $1")
        .bind(order_id)
        .fetch_one(pool)
        .await
        .context("could not read audit_log")?;

    Ok(count)
}

/// Whether the outbox row for an order has been published.
async fn outbox_published(pool: &sqlx::PgPool, order_id: uuid::Uuid) -> Result<bool> {
    // The outbox stores the whole envelope as JSONB, so the order id is inside
    // `payload -> data -> order_id` rather than in a column of its own.
    let published: Option<bool> = sqlx::query_scalar(
        "SELECT published_at IS NOT NULL
           FROM outbox
          WHERE payload -> 'data' ->> 'order_id' = $1
          ORDER BY created_at DESC
          LIMIT 1",
    )
    .bind(order_id.to_string())
    .fetch_optional(pool)
    .await
    .context("could not read the outbox")?;

    Ok(published.unwrap_or(false))
}

#[tokio::test]
#[ignore = "needs a composed stack: run DevStart.cmd, then cargo test -p e2e -- --ignored"]
async fn an_order_travels_from_the_gateway_to_the_audit_table() -> Result<()> {
    let harness = Harness::new();
    let pool = harness.database().await?;

    let order_id = harness.place_order("e2e-widget", 2, None).await?;

    // 1. The outbox row is written and then marked published by the relay.
    wait_until("the outbox row to be relayed", || async {
        outbox_published(&pool, order_id).await
    })
    .await?;

    // 2. The audit service reacted, which can only happen if the worker
    //    processed the command and published the event first. One assertion
    //    covers the whole chain.
    wait_until("an audit row for the order", || async {
        Ok(audit_rows(&pool, order_id).await? == 1)
    })
    .await?;

    // 3. The row records which worker did it, proving the event carried the
    //    processing detail rather than the auditor inventing it.
    let processed_by: String =
        sqlx::query_scalar("SELECT processed_by FROM audit_log WHERE order_id = $1")
            .bind(order_id)
            .fetch_one(&pool)
            .await?;

    assert!(
        !processed_by.is_empty(),
        "the audit row must name the worker that processed the order"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "needs a composed stack: run DevStart.cmd, then cargo test -p e2e -- --ignored"]
async fn both_subscribers_react_to_one_event() -> Result<()> {
    // The distinction this whole system exists to demonstrate: notifier and
    // audit hold *separate* durable consumer names, so each receives every
    // event rather than sharing them out.
    let harness = Harness::new();

    let before_notifier = harness
        .prometheus_scalar("sum(events_handled_total{service=\"notifier-service\"})")
        .await?
        .unwrap_or(0.0);
    let before_audit = harness
        .prometheus_scalar("sum(events_handled_total{service=\"audit-service\"})")
        .await?
        .unwrap_or(0.0);

    harness.place_order("e2e-fanout", 1, None).await?;

    wait_until(
        "both subscribers to have handled one more event",
        || async {
            let notifier = harness
                .prometheus_scalar("sum(events_handled_total{service=\"notifier-service\"})")
                .await?
                .unwrap_or(0.0);
            let audit = harness
                .prometheus_scalar("sum(events_handled_total{service=\"audit-service\"})")
                .await?
                .unwrap_or(0.0);

            // Both must advance. If only one moves they are sharing a consumer,
            // which would be messaging rather than eventing.
            Ok(notifier > before_notifier && audit > before_audit)
        },
    )
    .await?;

    Ok(())
}

#[tokio::test]
#[ignore = "needs a composed stack: run DevStart.cmd, then cargo test -p e2e -- --ignored"]
async fn the_same_order_submitted_twice_is_processed_once() -> Result<()> {
    let harness = Harness::new();
    let pool = harness.database().await?;

    // A fresh id, so this test cannot collide with a previous run's data.
    let order_id = uuid::Uuid::new_v4();

    harness
        .place_order("e2e-duplicate", 1, Some(order_id))
        .await?;

    wait_until("the first submission to be audited", || async {
        Ok(audit_rows(&pool, order_id).await? == 1)
    })
    .await?;

    // Same business key again. The gateway accepts it - re-submitting is not
    // an error - but the worker must recognise the order as already done.
    harness
        .place_order("e2e-duplicate", 1, Some(order_id))
        .await?;

    // Give the second submission time to travel the whole path before
    // asserting nothing extra happened. There is no event to wait *for* here,
    // so a fixed pause is the honest way to test an absence.
    tokio::time::sleep(std::time::Duration::from_secs(8)).await;

    let rows = audit_rows(&pool, order_id).await?;
    assert_eq!(
        rows, 1,
        "the duplicate must not produce a second audit row; idempotency failed"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "needs a composed stack: run DevStart.cmd, then cargo test -p e2e -- --ignored"]
async fn a_poison_order_is_retried_then_parked_without_blocking_the_queue() -> Result<()> {
    let harness = Harness::new();
    let pool = harness.database().await?;

    let before = harness.stream_messages("ORDER_DLQ").await?;

    // `POISON_ITEM` is the contract's agreed way to make the worker fail, so
    // this test does not depend on a magic string of its own.
    harness
        .place_order(messaging_core::contract::POISON_ITEM, 1, None)
        .await?;

    wait_until(
        "the poison order to reach the dead-letter stream",
        || async { Ok(harness.stream_messages("ORDER_DLQ").await? > before) },
    )
    .await?;

    // The queue must still be flowing. A dead letter that blocks everything
    // behind it is the failure mode dead-lettering exists to prevent, so prove
    // a normal order still completes afterwards.
    let follow_up = harness.place_order("e2e-after-poison", 1, None).await?;

    wait_until(
        "a normal order to complete after the poison one",
        || async { Ok(audit_rows(&pool, follow_up).await? == 1) },
    )
    .await?;

    Ok(())
}

#[tokio::test]
#[ignore = "needs a composed stack: run DevStart.cmd, then cargo test -p e2e -- --ignored"]
async fn the_stored_outbox_payload_matches_the_shared_contract() -> Result<()> {
    // The outbox is where the contract becomes durable. If what the gateway
    // stores stops matching `messaging-core`, the relay will publish something
    // no consumer can read - and this catches it at the boundary rather than
    // three services later.
    let harness = Harness::new();
    let pool = harness.database().await?;

    let order_id = harness.place_order("e2e-contract", 3, None).await?;

    wait_until("the outbox row to exist", || async {
        let found: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM outbox WHERE payload -> 'data' ->> 'order_id' = $1 LIMIT 1",
        )
        .bind(order_id.to_string())
        .fetch_optional(&pool)
        .await?;
        Ok(found.is_some())
    })
    .await?;

    let payload: serde_json::Value = sqlx::query_scalar(
        "SELECT payload FROM outbox WHERE payload -> 'data' ->> 'order_id' = $1 LIMIT 1",
    )
    .bind(order_id.to_string())
    .fetch_one(&pool)
    .await?;

    // The real assertion: the stored JSON deserialises into the shared type.
    // Turbofish because the target type cannot be inferred from the JSON.
    let envelope = serde_json::from_value::<Envelope<OrderCommand>>(payload)
        .context("the stored outbox payload no longer matches messaging-core")?;

    assert_eq!(envelope.data.order_id, order_id);
    assert_eq!(envelope.data.item, "e2e-contract");
    assert_eq!(envelope.data.quantity, 3);
    assert!(
        envelope.is_supported(),
        "the gateway must stamp a schema version this build understands"
    );
    assert!(
        !envelope.trace.is_empty(),
        "the trace context must be stored with the message, or the trace breaks at the outbox"
    );

    Ok(())
}
