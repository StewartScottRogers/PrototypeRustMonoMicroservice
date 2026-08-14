//! Connecting to NATS, declaring the streams, publishing, and consuming.

use crate::envelope::Envelope;
use crate::subjects;
use anyhow::{Context as _, Result};
use async_nats::jetstream::consumer::{DeliverPolicy, pull};
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
        // gone. JetStream adds the persistent log that makes durable
        // consumers, redelivery and replay possible.
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
    /// Note the two `.await`s on the publish line. The first sends the
    /// message; the second waits for the server's acknowledgement. Dropping
    /// the second would make this fire-and-forget, which quietly undoes the
    /// durability JetStream is here to provide.
    ///
    /// `T: Serialize` is a *trait bound*: this method accepts any payload type
    /// that serde can turn into JavaScript Object Notation.
    pub async fn publish<T>(&self, subject: &'static str, envelope: &Envelope<T>) -> Result<()>
    where
        T: Serialize,
    {
        let payload = serde_json::to_vec(envelope).context("could not encode the message")?;

        // Stamp trace context into the headers so the consumer continues this
        // trace rather than starting an unrelated one.
        //
        // The envelope's own stored context wins when it has one. That matters
        // for the relay: the span it publishes under belongs to the relay, but
        // the trace that should own the message belongs to the Hypertext
        // Transfer Protocol request that created the outbox row, possibly
        // minutes earlier.
        let mut headers = async_nats::HeaderMap::new();
        if envelope.trace.is_empty() {
            crate::trace::inject_current(&mut headers);
        } else {
            crate::trace::inject_map(&mut headers, &envelope.trace);
        }

        self.jetstream
            .publish_with_headers(subject, headers, payload.into())
            .await
            .with_context(|| format!("could not publish to {subject}"))?
            .await
            .with_context(|| format!("{subject} was not acknowledged by JetStream"))?;

        tracing::info!(subject, message_id = %envelope.id, kind = %envelope.kind, "published");
        Ok(())
    }

    /// Deletes a durable consumer if it exists.
    ///
    /// `get_or_create_consumer` returns an existing consumer *as it is* — it
    /// never reconciles a changed configuration. A tool whose settings have
    /// been corrected therefore keeps running under the old ones until
    /// something removes the consumer. For a one-shot tool, starting clean
    /// every time is simpler than reasoning about which settings drifted.
    ///
    /// A consumer that does not exist is not an error; that is the desired end
    /// state either way.
    pub async fn delete_consumer(
        &self,
        stream_name: &'static str,
        durable_name: &str,
    ) -> Result<()> {
        let stream = self
            .jetstream
            .get_stream(stream_name)
            .await
            .with_context(|| format!("stream {stream_name} does not exist"))?;

        match stream.delete_consumer(durable_name).await {
            Ok(_) => tracing::info!(consumer = durable_name, "removed the previous consumer"),
            Err(error) => tracing::debug!(%error, consumer = durable_name, "no consumer to remove"),
        }

        Ok(())
    }

    /// Opens a durable consumer and returns its message stream.
    ///
    /// # The one line that decides messaging vs eventing
    ///
    /// `durable_name`. Every process using the *same* name shares one position
    /// in the stream, so each message goes to exactly one of them — messaging.
    /// Processes using *different* names each get their own position, so all
    /// of them see every message — eventing.
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

    /// Opens a durable consumer that starts at the *end* of the stream.
    ///
    /// # Why this exists separately from [`Self::durable_consumer`]
    ///
    /// A normal consumer starts at the beginning and works forward, which is
    /// what a worker wants: nothing committed may be skipped. An **observer**
    /// wants the opposite. The mimic panel exists to answer "what is happening
    /// now", and these streams retain ten thousand messages — so a panel that
    /// started from the beginning would open by replaying every order this
    /// stack has ever seen, at full speed, as though it were all happening at
    /// once. Every restart would produce the same false stampede.
    ///
    /// `DeliverPolicy::New` means "deliver messages published from this moment
    /// onward". Anything already in the stream is skipped, deliberately.
    ///
    /// # Do not use this for work
    ///
    /// Skipping is data loss for anything that must not miss a message. This is
    /// for a display, and the only caller is the panel's tap. A worker, a
    /// subscriber, or anything that writes to a database must use
    /// [`Self::durable_consumer`].
    ///
    /// # The Rust in this signature
    ///
    /// `&'static str` for the names is a *lifetime bound* meaning "a string
    /// that lives for the whole program" — in practice a literal or a `const`.
    /// It costs nothing to pass and makes it impossible to hand in a temporary
    /// string that is freed while the consumer is still running.
    pub async fn durable_consumer_from_now(
        &self,
        stream_name: &'static str,
        durable_name: &'static str,
        filter_subject: &'static str,
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
                    // One attempt, never zero. `max_deliver` is skipped from the
                    // wire when it holds its default, so a 0 here would omit the
                    // field and let the server apply *unlimited* redelivery —
                    // the exact opposite of the intent. An observer must never
                    // cause a message to be redelivered.
                    max_deliver: 1,
                    ack_wait: ACK_WAIT,
                    deliver_policy: DeliverPolicy::New,
                    ..Default::default()
                },
            )
            .await
            .with_context(|| format!("could not create consumer {durable_name}"))?;

        tracing::info!(
            stream = stream_name,
            consumer = durable_name,
            filter = filter_subject,
            "observer consumer ready, starting from now"
        );

        consumer
            .messages()
            .await
            .with_context(|| format!("could not read from consumer {durable_name}"))
    }
}
