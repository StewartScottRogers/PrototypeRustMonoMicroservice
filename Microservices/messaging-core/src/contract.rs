//! The message shapes every service agrees on.
//!
//! These types live in a shared crate rather than being duplicated per service.
//! That is the monorepo paying for itself: change a field here and the compiler
//! finds every producer and consumer that needs updating, instead of a decoding
//! error appearing in production three weeks later.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A command: "process this order."
///
/// Commands are imperative and addressed to whoever does that work. Exactly one
/// worker handles each.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrderCommand {
    pub order_id: Uuid,
    pub item: String,
    pub quantity: u32,
}

/// An event: "this order completed."
///
/// Events are past tense and addressed to nobody in particular. The publisher
/// does not know or care who is listening, which is exactly what lets you add a
/// subscriber without touching the publisher.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrderCompleted {
    pub order_id: Uuid,
    pub item: String,
    pub quantity: u32,
    /// Which worker instance handled it. With two workers running, this field
    /// is the visible proof that the load actually split.
    pub processed_by: String,
}

/// The magic word that makes the worker fail on purpose.
///
/// Needed to demonstrate retry and the dead-letter queue: a failure you can
/// trigger on demand beats waiting for a real one.
pub const POISON_ITEM: &str = "poison";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_survives_json() {
        let command = OrderCommand {
            order_id: Uuid::new_v4(),
            item: "widget".to_owned(),
            quantity: 3,
        };

        let decoded: OrderCommand =
            serde_json::from_slice(&serde_json::to_vec(&command).unwrap()).unwrap();

        assert_eq!(command, decoded);
    }

    #[test]
    fn an_event_survives_json() {
        let event = OrderCompleted {
            order_id: Uuid::new_v4(),
            item: "widget".to_owned(),
            quantity: 3,
            processed_by: "worker-1".to_owned(),
        };

        let decoded: OrderCompleted =
            serde_json::from_slice(&serde_json::to_vec(&event).unwrap()).unwrap();

        assert_eq!(event, decoded);
    }
}
