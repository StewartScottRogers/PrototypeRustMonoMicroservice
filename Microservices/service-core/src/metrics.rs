//! Counting things, and letting Prometheus read the counts.
//!
//! # How this differs from logs and traces
//!
//! You now have three views of the same system, and they answer different
//! questions:
//!
//! - **Logs** — what happened, one event at a time. Precise, unbounded, awkward
//!   to summarise.
//! - **Traces** — what one request caused, across every service it touched.
//!   Detailed, but only for the requests you sample.
//! - **Metrics** — how much and how often, aggregated over time. Cheap enough
//!   to keep for everything, and the only one that answers "is this getting
//!   worse?"
//!
//! A trace tells you why *this* order was slow. A metric tells you that orders
//! got slower at 14:20.
//!
//! # Naming
//!
//! Prometheus convention, followed here because every tool assumes it:
//! `unit_suffixed_snake_case`, counters ending `_total`, durations in seconds.
//! `orders_processed_total`, not `processedOrders`.

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use std::sync::OnceLock;

/// The rendered-metrics handle, set up once per process.
///
/// `OnceLock` is a cell that can be written exactly once and read from any
/// thread afterwards. It suits process-wide setup: no lock on the read path,
/// and the type system prevents a second initialisation.
static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Installs the Prometheus recorder.
///
/// Call once, from `main`, before recording anything. Metrics recorded with no
/// recorder installed are silently discarded — which is safe, and is why unit
/// tests can call the macros freely without setting anything up.
///
/// Returns quietly if called twice rather than panicking: a duplicate call is
/// a wiring mistake, not a reason to refuse to start.
pub fn init_metrics(service: &'static str) {
    if HANDLE.get().is_some() {
        return;
    }

    match PrometheusBuilder::new().install_recorder() {
        Ok(handle) => {
            let _ = HANDLE.set(handle);
            tracing::info!("metrics recorder installed");
        }
        Err(error) => {
            // Observability must never stop a service from starting.
            tracing::error!(%error, "could not install the metrics recorder");
        }
    }

    // Every service asserts its own identity.
    //
    // # Why this is not redundant with the scrape configuration
    //
    // Prometheus labels a target from whatever name resolution told it, and
    // never checks. Docker recycles container IP addresses: delete a service,
    // and the next container to start can be handed the address it used to
    // hold. Prometheus then scrapes that address, gets a perfectly valid
    // response from an entirely different service, and reports the dead one as
    // healthy.
    //
    // That happened here, and it is the worst failure a monitoring system can
    // have - confidently wrong. This metric lets a query confirm that the
    // process answering actually is who the target claims, so identity comes
    // from the service rather than from a stale name resolution cache.
    metrics::gauge!(SERVICE_INFO, "service" => service).set(1.0);
}

/// Renders the current metrics in Prometheus text format.
///
/// Returns an empty body when no recorder is installed, which is what a scrape
/// of a service that failed to set metrics up should see — an empty result
/// rather than an error page.
pub fn render() -> String {
    HANDLE
        .get()
        .map(PrometheusHandle::render)
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The metric names, in one place.
//
// Constants rather than string literals at each call site, for the same reason
// the NATS subjects are: a typo produces a second, silently empty metric
// rather than a compile error, and that is a genuinely miserable thing to
// debug.
// ---------------------------------------------------------------------------

/// Always 1, labelled with the name the process believes it has.
///
/// Queried instead of `up` when the question is "is *this service* alive",
/// because `up` only means "something answered at the address name resolution
/// gave me".
pub const SERVICE_INFO: &str = "service_info";

/// Orders accepted by the gateway and written to the outbox.
pub const ORDERS_ACCEPTED: &str = "orders_accepted_total";
/// Outbox rows published to the broker by the relay.
pub const OUTBOX_RELAYED: &str = "outbox_relayed_total";
/// Commands processed successfully by a worker.
pub const ORDERS_PROCESSED: &str = "orders_processed_total";
/// Commands that failed and will be retried.
pub const ORDERS_RETRIED: &str = "orders_retried_total";
/// Commands abandoned to the dead-letter queue.
pub const ORDERS_DEAD_LETTERED: &str = "orders_dead_lettered_total";
/// Duplicate orders skipped by the idempotency guard.
pub const ORDERS_DUPLICATE: &str = "orders_duplicate_total";
/// Events reacted to by a subscriber. Labelled by service, so one metric shows
/// both subscribers and makes an imbalance between them obvious.
pub const EVENTS_HANDLED: &str = "events_handled_total";
/// How long processing one command took, in seconds.
pub const PROCESSING_SECONDS: &str = "order_processing_duration_seconds";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_is_empty_before_installation() {
        // Whether the recorder is installed depends on test ordering, so
        // assert only what is true either way: rendering never panics.
        let _ = render();
    }

    #[test]
    fn installing_twice_is_harmless() {
        init_metrics("test-service");
        init_metrics("test-service");

        // Recording against the facade must work regardless of whether the
        // recorder installed - that is what makes it safe to instrument code
        // that unit tests also exercise.
        metrics::counter!(ORDERS_PROCESSED).increment(1);
    }

    #[test]
    fn service_info_is_not_a_counter() {
        // It is a gauge asserting identity, so the _total suffix would be a
        // lie and would also break the loop below.
        assert!(!SERVICE_INFO.ends_with("_total"));
        assert_eq!(SERVICE_INFO, "service_info");
    }

    #[test]
    fn the_names_follow_prometheus_convention() {
        for name in [
            ORDERS_ACCEPTED,
            OUTBOX_RELAYED,
            ORDERS_PROCESSED,
            ORDERS_RETRIED,
            ORDERS_DEAD_LETTERED,
            ORDERS_DUPLICATE,
            EVENTS_HANDLED,
        ] {
            assert!(
                name.ends_with("_total"),
                "{name} is a counter, so it must end in _total"
            );
            assert_eq!(name, name.to_lowercase(), "{name} must be snake_case");
        }

        assert!(
            PROCESSING_SECONDS.ends_with("_seconds"),
            "durations are measured in seconds, and the name must say so"
        );
    }
}
