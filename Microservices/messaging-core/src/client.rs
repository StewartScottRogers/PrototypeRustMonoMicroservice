//! Connecting to NATS, declaring the streams, publishing, and consuming.

use crate::envelope::Envelope;
use crate::subjects;
use anyhow::{Context as _, Result};
use async_nats::jetstream::consumer::pull;
use async_nats::jetstream::{self, stream};
use serde::Serialize;
use std::time::Duration;

/// How long JetStream waits for an ack before redelivering a message.
const ACK_WAIT: Duration = Duration::from_secs(10);

/// A connection to NATS, with JetStream enabled.
///
/// Cheap to clone: the underlying client is a handle to one shared connection,
/// the same arrangement as `reqwest::Client` in the gateway.
#[derive(Clone)]
pub struct Messaging {
    jetstream: jetstream::Context,
}

impl Messaging {
    /// Connects to `url`, e.g. `nats://nats:4222`.
    ///
    /// `anyhow::Result<Self>` is "either a Messaging or an error with context
    /// attached". `.context(...)` adds a human-readable layer to whatever the
    /// library reported, so a failure says *what we were trying to do*, not
    /// just "connection refused".
    pub async fn connect(url: &str) -> Result<Self> {
        let client = async_nats::connect(url)
            .await
            .with_context(|| format!("could not connect to NATS at {url}"))?;

        // Plain NATS is fire-and-forget: if nobody is listening the message is
        // gone. JetStream adds the persistent log that makes durable consumers,
        // redelivery and replay possible.
        Ok(Self {
            jetstream: jetstream::new(client),
        })
    }

    /// Creates the three streams if they do not already exist.
    ///
    /// Safe to call from every service on every start — `get_or_create_stream`
    /// is idempotent. That matters because there is no migration step for a
    /// broker; whoever starts first sets it up.
    pub async fn ensure_streams(&self) -> Result<()> {
        let streams = [
            (
                subjects::STREAM_COMMANDS,
                subjects::STREAM_COMMANDS_SUBJECTS,
            ),
            (subjects::STREAM_EVENTS, subjects::STREAM_EVENTS_SUBJECTS),
            (
                subjects::STREAM_DEAD_LETTER,
                subjects::STREAM_DEAD_LETTER_SUBJECTS,
            ),
        ];

        for (name, subject) in streams {
            self.jetstream
                .get_or_create_stream(stream::Config {
                    name: name.to_owned(),
                    subjects: vec![subject.to_owned()],
                    // A demo stack should not fill a laptop's disk.
                    max_messages: 10_000,
                    ..Default::default()
                })
                .await
                .with_context(|| format!("could not declare stream {name}"))?;

            tracing::info!(stream = name, subject, "stream ready");
        }

        Ok(())
    }

    /// Publishes an envelope and waits for JetStream to confirm it is stored.
    ///
    /// Note the two `.await`s on the publish line. The first sends the message;
    /// the second waits for the server's acknowledgement. Dropping the second
    /// would make this fire-and-forget, which quietly undoes the durability
    /// JetStream is here to provide.
    ///
    /// `T: Serialize` is a *trait bound*: this method accepts any payload type
    /// that serde can turn into JSON.
    pub async fn publish<T>(&self, subject: &'static str, envelope: &Envelope<T>) -> Result<()>
    where
        T: Serialize,
    {
        let payload = serde_json::to_vec(envelope).context("could not encode the message")?;

        // Stamp the current trace context into the headers so the consumer can
        // continue this trace rather than starting an unrelated one.
        let mut headers = async_nats::HeaderMap::new();
        crate::trace::inject_current(&mut headers);

        self.jetstream
            .publish_with_headers(subject, headers, payload.into())
            .await
            .with_context(|| format!("could not publish to {subject}"))?
            .await
            .with_context(|| format!("{subject} was not acknowledged by JetStream"))?;

        tracing::info!(subject, message_id = %envelope.id, kind = %envelope.kind, "published");
        Ok(())
    }

    /// Opens a durable consumer and returns its message stream.
    ///
    /// # The one line that decides messaging vs eventing
    ///
    /// `durable_name`. Every process using the *same* name shares one position
    /// in the stream, so each message goes to exactly one of them — messaging.
    /// Processes using *different* names each get their own position, so all of
    /// them see every message — eventing.
    ///
    /// `max_deliver` caps redelivery: after that many failed attempts the
    /// worker routes the message to the dead-letter stream instead of looping
    /// on it forever.
    pub async fn durable_consumer(
        &self,
        stream_name: &'static str,
        durable_name: &'static str,
        filter_subject: &'static str,
        max_deliver: i64,
    ) -> Result<pull::Stream> {
        let stream = self
            .jetstream
            .get_stream(stream_name)
            .await
            .with_context(|| format!("stream {stream_name} does not exist"))?;

        let consumer = stream
            .get_or_create_consumer(
                durable_name,
                pull::Config {
                    durable_name: Some(durable_name.to_owned()),
                    filter_subject: filter_subject.to_owned(),
                    max_deliver,
                    ack_wait: ACK_WAIT,
                    ..Default::default()
                },
            )
            .await
            .with_context(|| format!("could not create consumer {durable_name}"))?;

        tracing::info!(
            stream = stream_name,
            consumer = durable_name,
            filter = filter_subject,
            "consumer ready"
        );

        consumer
            .messages()
            .await
            .with_context(|| format!("could not read from consumer {durable_name}"))
    }
}
