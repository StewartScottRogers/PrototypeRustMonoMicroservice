//! Carrying trace context across the broker.
//!
//! # The problem
//!
//! A trace is a tree of spans tied together by a trace id. Within one process
//! that is automatic. Across a network hop it is not: the receiving process has
//! no idea the sender existed unless the sender says so, in the message.
//!
//! HTTP solves this with a `traceparent` header. NATS messages have headers
//! too, so the same W3C standard works — it just has to be wired up by hand.
//!
//! Without this module every service produces its own disconnected trace, and
//! the one question worth asking of an event-driven system — "what did this
//! request actually cause?" — has no answer.
//!
//! ```text
//!   gateway span ──traceparent in a NATS header──▶ worker span
//!                                                      │
//!                                        ──▶ notifier span, audit span
//! ```

use async_nats::HeaderMap;
use opentelemetry::propagation::{Extractor, Injector};
use std::str::FromStr as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

/// The header carrying trace context, from the W3C Trace Context standard.
/// Named here only for documentation — the propagator writes it itself.
pub const TRACEPARENT: &str = "traceparent";
/// Vendor-specific trace state, the other half of the W3C standard.
pub const TRACESTATE: &str = "tracestate";

/// Writes the current span's trace context into `headers`.
///
/// Call this immediately before publishing. If no trace is active, or no
/// exporter is configured, it writes nothing and costs nothing.
pub fn inject_current(headers: &mut HeaderMap) {
    let context = tracing::Span::current().context();

    opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&context, &mut NatsInjector(headers));
    });
}

/// Builds a span for a received message, parented to whatever produced it.
///
/// The returned span is *not* entered — pass it to `.instrument(span)` on the
/// async block that handles the message, so it covers the whole handling and
/// not just the moment of creation.
pub fn span_for_message(name: &'static str, headers: Option<&HeaderMap>) -> tracing::Span {
    let span = tracing::info_span!("message", handler = name);

    if let Some(headers) = headers {
        let parent = opentelemetry::global::get_text_map_propagator(|propagator| {
            propagator.extract(&NatsExtractor(headers))
        });
        // This is the join: the new span becomes a child of the publisher's,
        // so both appear in one trace rather than two unrelated ones.
        //
        // The result is deliberately discarded. It fails when no tracing
        // subscriber is installed - true in unit tests and when the collector
        // is not configured - and a missing trace must never stop a message
        // from being handled.
        let _ = span.set_parent(parent);
    }

    span
}

/// Adapter letting the propagator write into a NATS header map.
struct NatsInjector<'a>(&'a mut HeaderMap);

impl Injector for NatsInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        // A header name the NATS client rejects is not worth failing a publish
        // over - losing a trace is better than losing a message.
        if let Ok(name) = async_nats::HeaderName::from_str(key) {
            self.0.insert(name, value.as_str());
        }
    }
}

/// Adapter letting the propagator read from a NATS header map.
struct NatsExtractor<'a>(&'a HeaderMap);

impl Extractor for NatsExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(|value| value.as_str())
    }

    fn keys(&self) -> Vec<&str> {
        // async-nats keeps `HeaderName::as_str` private, so the real key list
        // cannot be enumerated. That is fine here: the W3C propagator reads its
        // fields by name through `get`, and never calls this. Naming the two
        // fields it could ask for keeps the contract honest.
        vec![TRACEPARENT, TRACESTATE]
    }
}
