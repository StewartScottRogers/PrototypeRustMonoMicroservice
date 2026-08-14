//! Process-wide tracing setup: structured logs, plus spans exported to Jaeger.
//!
//! "Tracing" here means two things at once, and they share one API:
//!
//! - **Structured logs** — each record is a set of named fields, not a
//!   sentence, so a log system can index and query them.
//! - **Distributed traces** — spans stitched together across services, so one
//!   request that crosses four processes and a message broker reads as a single
//!   timeline.
//!
//! The second is what makes eventing debuggable. Publishing an event decouples
//! the publisher from its subscribers, which is the point — but it also hides
//! the causal chain. A trace gives it back.

use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

/// Installs the subscriber for `service`.
///
/// If `OTEL_EXPORTER_OTLP_ENDPOINT` is set, spans are also exported there —
/// `http://jaeger:4317` in the compose stack. If it is unset, or the collector
/// is unreachable, the service logs normally and simply produces no traces.
/// Observability must never be the reason a service fails to start.
///
/// Call once, from `main`. A second call panics inside `tracing`.
pub fn init_tracing(service: &'static str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let json_layer = fmt::layer().json().flatten_event(true);

    // `Option<Layer>` is itself a valid layer: `None` contributes nothing. That
    // is how the same registry works both with and without a collector,
    // without duplicating the whole builder chain.
    let otel_layer = otel_layer(service);

    tracing_subscriber::registry()
        .with(filter)
        .with(json_layer)
        .with(otel_layer)
        .init();

    tracing::info!(service, "tracing initialised");
}

/// Builds the OpenTelemetry layer, or `None` if tracing is not configured.
fn otel_layer<S>(
    service: &'static str,
) -> Option<tracing_opentelemetry::OpenTelemetryLayer<S, opentelemetry_sdk::trace::Tracer>>
where
    S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
{
    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok()?;

    // The propagator decides the wire format for "which trace is this part of".
    // W3C traceparent is the standard one, and it is what messaging-core writes
    // into NATS headers so a trace survives the hop through the broker.
    opentelemetry::global::set_text_map_propagator(
        opentelemetry_sdk::propagation::TraceContextPropagator::new(),
    );

    // `with_endpoint` is a trait method, so the trait must be in scope for it
    // to be callable - the same rule that made `tower::ServiceExt` necessary
    // for `.oneshot()` in the tests.
    use opentelemetry_otlp::WithExportConfig as _;

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(&endpoint)
        .build()
        .inspect_err(|error| {
            eprintln!("tracing disabled: could not build the OTLP exporter: {error}");
        })
        .ok()?;

    // A *batch* exporter sends spans on a background task. The alternative
    // blocks the request that produced the span on a network call to the
    // collector, which is a bad trade.
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(
            opentelemetry_sdk::Resource::builder()
                // This is the name in Jaeger's service dropdown.
                .with_service_name(service)
                .build(),
        )
        .build();

    use opentelemetry::trace::TracerProvider as _;
    let tracer = provider.tracer(service);

    // Handing the provider to the global registry keeps it alive; dropping it
    // here would shut the exporter down immediately and silently.
    opentelemetry::global::set_tracer_provider(provider);

    eprintln!("tracing: exporting spans to {endpoint}");
    Some(tracing_opentelemetry::layer().with_tracer(tracer))
}
