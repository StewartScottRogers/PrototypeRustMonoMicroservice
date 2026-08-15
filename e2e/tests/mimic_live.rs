//! End-to-end: the panel reflects real traffic.
//!
//! # Why these assert through `/api/state` and not the socket
//!
//! The panel is pushed to over a WebSocket, and it would be natural to test the
//! socket directly. These tests deliberately do not, for two reasons.
//!
//! First, a WebSocket client is a dependency, and this crate has stayed free of
//! one. Second, and more usefully: `/api/state` serves the *same merged
//! snapshot* the socket pushes, from the same watch channel. Asserting through
//! it proves the whole pipeline — tap, counters, merge rule — produced the
//! right picture, without also testing a transport that the browser already
//! exercises every second of every day the stack is up.
//!
//! What is left untested here, said plainly: that the socket delivers. That is
//! checked by opening the panel, which is what the manual step in the pull
//! request description covers.
//!
//! ```text
//! DevStart.cmd
//! cargo test -p e2e -- --ignored --test-threads=1
//! ```

use anyhow::{Context as _, Result};
use e2e::{Harness, wait_until};

/// Reads one reading out of the panel's own snapshot.
///
/// Returns `None` when the panel has no such reading, which is a different
/// thing from a reading of zero and is worth keeping distinct.
async fn reading(harness: &Harness, id: &str) -> Result<Option<String>> {
    let body: serde_json::Value = reqwest::get(format!("{}/api/state", harness.endpoints.mimic))
        .await
        .context("the mimic panel did not answer — is the stack running?")?
        .json()
        .await
        .context("the panel's state was not JavaScript Object Notation")?;

    let found = body
        .get("gauges")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .find(|gauge| gauge.get("id").and_then(serde_json::Value::as_str) == Some(id))
        .and_then(|gauge| gauge.get("value"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);

    Ok(found)
}

#[tokio::test]
#[ignore = "needs a composed stack: run DevStart.cmd, then cargo test -p e2e -- --ignored"]
async fn the_panel_counts_orders_it_watched_cross_the_bus() -> Result<()> {
    let harness = Harness::new();

    // The reading the tap owns. It should be a number from the moment the panel
    // starts, never absent, because the tap counts locally rather than asking
    // anything.
    let before = reading(&harness, "g-processed")
        .await?
        .context("the panel has no reading for processed orders")?;

    for index in 0..5 {
        harness
            .place_order(&format!("mimic-live-{index}"), 1, None)
            .await?;
    }

    // Pushed from the message bus rather than sampled, so this is quick — but
    // still polled, because "quick" is not "synchronous".
    wait_until("the panel to count the orders it saw", || async {
        let now = reading(&harness, "g-processed").await?;
        Ok(now.as_deref() != Some(before.as_str()) && now.is_some())
    })
    .await?;

    Ok(())
}

#[tokio::test]
#[ignore = "needs a composed stack: run DevStart.cmd, then cargo test -p e2e -- --ignored"]
async fn the_panel_never_reports_a_service_down_while_it_is_answering() -> Result<()> {
    // The failure this guards against is the one that would make the whole
    // panel worthless: the flow plane influencing the health plane, so that a
    // quiet message bus painted six healthy services as dead.
    let harness = Harness::new();

    let body: serde_json::Value = reqwest::get(format!("{}/api/state", harness.endpoints.mimic))
        .await?
        .json()
        .await?;

    let nodes = body
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .context("the panel reported no nodes at all")?;

    for node in nodes {
        let id = node["id"].as_str().unwrap_or("unnamed");
        let status = node["status"].as_str().unwrap_or("missing");

        assert_ne!(
            status, "down",
            "the panel says {id} is down while the stack is up; \
             if this fails after a change to the merge rule, the flow plane has \
             started influencing the health plane"
        );
    }

    Ok(())
}
