//! The wrapper every message travels in.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A message plus the metadata that makes it traceable and de-duplicatable.
///
/// # Why wrap the payload at all
///
/// Sending the bare payload works right up until you need to answer "have I
/// already seen this?" or "when did this happen?" — and by then every producer
/// and consumer has to change. The envelope is the cheap insurance.
///
/// # Rust concepts here
///
/// `Envelope<T>` is *generic*: `T` is a placeholder for whatever payload type
/// you use, filled in at the call site (`Envelope<OrderCommand>`). One
/// definition serves every message type, checked at compile time.
///
/// The `where` clause on the impl block below is a *trait bound*: it says these
/// methods only exist when `T` can be turned into JSON.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Envelope<T> {
    /// Unique per message. The worker uses this to detect a redelivery it has
    /// already handled — see the idempotency guard.
    pub id: Uuid,
    /// What kind of message this is, for humans reading logs.
    pub kind: String,
    /// When the thing actually happened, not when it was delivered.
    pub occurred_at: DateTime<Utc>,
    /// The actual message.
    pub data: T,
}

impl<T> Envelope<T> {
    /// Wraps `data`, stamping a fresh id and the current time.
    ///
    /// `impl Into<String>` accepts anything convertible to a `String` — both a
    /// `&str` literal and an owned `String` — so callers need no `.to_owned()`.
    pub fn new(kind: impl Into<String>, data: T) -> Self {
        Self {
            id: Uuid::new_v4(),
            kind: kind.into(),
            occurred_at: Utc::now(),
            data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct Payload {
        value: String,
    }

    #[test]
    fn new_stamps_an_id_and_a_time() {
        let before = Utc::now();
        let envelope = Envelope::new(
            "test.kind",
            Payload {
                value: "hello".to_owned(),
            },
        );

        assert_eq!(envelope.kind, "test.kind");
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
        let original = Envelope::new(
            "test.kind",
            Payload {
                value: "hello".to_owned(),
            },
        );

        let bytes = serde_json::to_vec(&original).unwrap();
        // The turbofish tells serde which type to rebuild; it cannot be
        // inferred from the bytes alone.
        let decoded = serde_json::from_slice::<Envelope<Payload>>(&bytes).unwrap();

        assert_eq!(original, decoded);
    }
}
