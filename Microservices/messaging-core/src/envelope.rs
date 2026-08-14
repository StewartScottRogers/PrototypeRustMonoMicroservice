//! The wrapper every message travels in.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

/// The schema version every service in this workspace currently speaks.
///
/// # Why a version at all
///
/// A stream holds messages for as long as its retention says. Deploy a change
/// to a payload shape and there are, for a while, messages of *both* shapes in
/// flight. Without a version the consumer's only signal is a decode failure,
/// which is indistinguishable from corruption.
///
/// The rule: **additive changes keep the version; anything that could break an
/// existing consumer increments it.** Adding an optional field is additive.
/// Renaming a field, removing one, or changing its type is not.
pub const SCHEMA_VERSION: u32 = 1;

/// A message plus the metadata that makes it traceable, versioned and
/// de-duplicatable.
///
/// # Why wrap the payload at all
///
/// Sending the bare payload works right up until you need to answer "have I
/// already seen this?", "when did this happen?" or "what request caused it?" —
/// and by then every producer and consumer has to change. The envelope is the
/// cheap insurance.
///
/// # Rust concepts here
///
/// `Envelope<T>` is *generic*: `T` is a placeholder for whatever payload type
/// you use, filled in at the call site (`Envelope<OrderCommand>`). One
/// definition serves every message type, checked at compile time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Envelope<T> {
    /// Unique per message.
    pub id: Uuid,
    /// What kind of message this is, for humans reading logs.
    pub kind: String,
    /// Which schema this message was written against. See [`SCHEMA_VERSION`].
    ///
    /// `#[serde(default = "...")]` supplies a value when the field is missing,
    /// so messages written before versioning existed still decode as version 1
    /// rather than failing outright.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// When the thing actually happened, not when it was delivered.
    pub occurred_at: DateTime<Utc>,
    /// World Wide Web Consortium trace context captured where the message was
    /// created.
    ///
    /// Carrying it *in the envelope* rather than only in the broker headers is
    /// what lets a trace survive the outbox: the envelope is stored as
    /// JavaScript Object Notation in Postgres, so the context is still there
    /// minutes later when the relay picks the row up on a different task.
    ///
    /// `BTreeMap` rather than `HashMap` so the JavaScript Object Notation
    /// field order is stable, which keeps stored rows diffable.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub trace: BTreeMap<String, String>,
    /// The actual message.
    pub data: T,
}

/// Serde needs a function, not a literal, for `default = "..."`.
fn default_schema_version() -> u32 {
    1
}

impl<T> Envelope<T> {
    /// Wraps `data`, stamping a fresh id, the current time, the current schema
    /// version, and whatever trace context is active right now.
    ///
    /// `impl Into<String>` accepts anything convertible to a `String` — both a
    /// `&str` literal and an owned `String` — so callers need no
    /// `.to_owned()`.
    pub fn new(kind: impl Into<String>, data: T) -> Self {
        Self {
            id: Uuid::new_v4(),
            kind: kind.into(),
            schema_version: SCHEMA_VERSION,
            occurred_at: Utc::now(),
            trace: crate::trace::current_context_map(),
            data,
        }
    }

    /// Whether this service can safely handle the message.
    ///
    /// A consumer that meets an unsupported version must not guess. Dropping
    /// it loses data and processing it risks acting on a misread payload;
    /// routing it to the dead-letter queue keeps it for a human.
    pub fn is_supported(&self) -> bool {
        self.schema_version == SCHEMA_VERSION
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct Payload {
        value: String,
    }

    fn payload() -> Payload {
        Payload {
            value: "hello".to_owned(),
        }
    }

    #[test]
    fn new_stamps_an_id_a_time_and_a_version() {
        let before = Utc::now();
        let envelope = Envelope::new("test.kind", payload());

        assert_eq!(envelope.kind, "test.kind");
        assert_eq!(envelope.schema_version, SCHEMA_VERSION);
        assert!(envelope.occurred_at >= before);
        assert_eq!(envelope.data.value, "hello");
    }

    #[test]
    fn every_envelope_gets_a_different_id() {
        let one = Envelope::new("k", ());
        let two = Envelope::new("k", ());
        assert_ne!(one.id, two.id, "ids must be unique or dedupe cannot work");
    }

    #[test]
    fn survives_a_round_trip_through_json() {
        let original = Envelope::new("test.kind", payload());

        let bytes = serde_json::to_vec(&original).unwrap();
        let decoded = serde_json::from_slice::<Envelope<Payload>>(&bytes).unwrap();

        assert_eq!(original, decoded);
    }

    #[test]
    fn the_current_version_is_supported() {
        assert!(Envelope::new("k", ()).is_supported());
    }

    #[test]
    fn a_future_version_is_not_supported() {
        let mut envelope = Envelope::new("k", ());
        envelope.schema_version = SCHEMA_VERSION + 1;
        assert!(
            !envelope.is_supported(),
            "a newer schema must be quarantined, not guessed at"
        );
    }

    #[test]
    fn a_message_written_before_versioning_decodes_as_version_one() {
        // Exactly what an older producer would have written: no
        // schema_version, no trace. It must still decode rather than fail.
        let legacy = r#"{
            "id": "0d4f0a4a-1f5b-4a2e-9f6a-1b2c3d4e5f60",
            "kind": "test.kind",
            "occurred_at": "2026-01-01T00:00:00Z",
            "data": { "value": "hello" }
        }"#;

        let decoded = serde_json::from_str::<Envelope<Payload>>(legacy).unwrap();

        assert_eq!(decoded.schema_version, 1);
        assert!(decoded.trace.is_empty());
        assert!(decoded.is_supported());
    }

    #[test]
    fn an_empty_trace_is_left_out_of_the_json() {
        let mut envelope = Envelope::new("k", payload());
        envelope.trace.clear();

        let json = serde_json::to_string(&envelope).unwrap();

        assert!(
            !json.contains("trace"),
            "an empty map should not bloat every stored row: {json}"
        );
    }
}
