//! The Postgres schema, in one place.
//!
//! # Why the migrations are not inside a service
//!
//! `sqlx` records which migrations it has applied in a `_sqlx_migrations`
//! table. If two services each embedded their own set, they would write
//! conflicting version numbers into that one table and fight.
//!
//! So the schema lives here, both services depend on this crate, and both run
//! the identical set. `sqlx` takes a Postgres advisory lock while migrating, so
//! two services starting at the same moment is safe: one applies the
//! migrations, the other waits and finds nothing to do.

use anyhow::{Context as _, Result};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

/// Connects and applies any outstanding migrations.
///
/// A *pool* rather than a single connection: work handled concurrently needs
/// more than one connection, and opening a fresh one per query is slow.
pub async fn connect_and_migrate(database_url: &str) -> Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await
        .context("could not connect to Postgres")?;

    // `sqlx::migrate!` reads the ./migrations directory *at compile time* and
    // embeds the SQL in the binary. That is why the container image needs no
    // .sql files and no separate migration step - a distroless image could not
    // run one anyway.
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("could not apply database migrations")?;

    tracing::info!("database ready");
    Ok(pool)
}
