//! Making a consumer safe to run twice on the same message.
//!
//! # Why this is not optional
//!
//! JetStream guarantees *at-least-once* delivery. If a worker processes a
//! message and dies before acknowledging it, the broker redelivers — correctly,
//! because from its side the work might never have happened. Duplicates are
//! therefore normal operation, not a bug to be fixed in the broker.
//!
//! The consumer has to be the one that copes. Either its work is naturally
//! repeatable, or it remembers what it has already done. This is the
//! remembering.
//!
//! Redis is the right shape for that memory: the check and the record must be a
//! single atomic step, which `SET NX` gives, and entries should expire on their
//! own rather than growing forever, which `EX` gives.

use anyhow::{Context as _, Result};
use std::time::Duration;
use uuid::Uuid;

/// How long a processed id is remembered. Long enough to cover any plausible
/// redelivery, short enough that the key set stays bounded.
const REMEMBER_FOR: Duration = Duration::from_secs(3600);

/// Remembers which message ids have already been handled.
///
/// Cloneable, like the other connection handles in this workspace: the inner
/// Redis connection is multiplexed, so clones share one socket.
#[derive(Clone)]
pub struct IdempotencyGuard {
    connection: redis::aio::MultiplexedConnection,
    /// Prefixed so two different consumers can each track the same message id
    /// independently — the notifier having seen an event says nothing about
    /// whether the auditor has.
    namespace: &'static str,
}

impl IdempotencyGuard {
    /// Connects to Redis, e.g. `redis://redis:6379`.
    pub async fn connect(url: &str, namespace: &'static str) -> Result<Self> {
        let client =
            redis::Client::open(url).with_context(|| format!("{url} is not a valid Redis URL"))?;

        let connection = client
            .get_multiplexed_async_connection()
            .await
            .with_context(|| format!("could not connect to Redis at {url}"))?;

        Ok(Self {
            connection,
            namespace,
        })
    }

    /// Returns `true` the first time it sees `id`, and `false` on every repeat.
    ///
    /// `SET key value NX EX seconds` is one round trip that both tests and
    /// records. Checking with `EXISTS` and then writing would leave a gap in
    /// which a second worker could pass the same check — the exact race this
    /// exists to prevent.
    ///
    /// `&mut self` because sending a command mutates the connection state; the
    /// compiler enforces that no two callers do so at the same instant.
    pub async fn first_time_seeing(&mut self, id: Uuid) -> Result<bool> {
        let key = format!("seen:{}:{}", self.namespace, id);

        // Redis replies "OK" when it set the key, or nil when NX blocked it.
        // Option<String> models exactly that: Some means we are the first.
        let outcome: Option<String> = redis::cmd("SET")
            .arg(&key)
            .arg(1)
            .arg("NX")
            .arg("EX")
            .arg(REMEMBER_FOR.as_secs())
            .query_async(&mut self.connection)
            .await
            .with_context(|| format!("could not record {key} in Redis"))?;

        Ok(outcome.is_some())
    }

    /// Undoes [`first_time_seeing`], so a failed attempt can be retried.
    ///
    /// Without this the guard would be a trap: the first delivery claims the
    /// key, processing fails, and every redelivery is then dismissed as a
    /// duplicate — the message is lost precisely because we tried to be safe.
    ///
    /// [`first_time_seeing`]: Self::first_time_seeing
    pub async fn forget(&mut self, id: Uuid) -> Result<()> {
        let key = format!("seen:{}:{}", self.namespace, id);

        redis::cmd("DEL")
            .arg(&key)
            .query_async::<()>(&mut self.connection)
            .await
            .with_context(|| format!("could not clear {key} in Redis"))?;

        Ok(())
    }
}
