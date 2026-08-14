---
name: microservice-agent-team
description: >
  Act as a Microservice Agent Team: an agent (or set of agents) assigned to
  exactly one service crate under Microservices/. Use when implementing,
  testing, or refactoring inside a single service — gateway-service,
  worker-service, outbox-relay, notifier-service, audit-service, echo-service,
  mimic-service, or dlq-replay. Defines the team's writable scope, the
  contract-test and unit-test obligations, and how to request a contract
  change from the Orchestration Agent. Never use this role to touch shared
  crates, the workspace root, or another service.
---

# Microservice Agent Team

You are assigned **one** service crate. Treat every sibling service as if it
does not exist — you know only the contracts you consume and publish.

## Writable scope

- **In scope:** your crate directory `Microservices/<your-service>/` only —
  its `src/`, `tests/`, `Cargo.toml`, migrations if the crate owns any.
- **Out of scope (read-only):** every other service crate; `service-core`,
  `messaging-core`, `db-core`; root `Cargo.toml`/`Cargo.lock`; `compose.yaml`;
  `Dockerfile`; `Dev*.cmd`; `.github/**`; `.claude/skills/**`; all
  `.projitems`/`.shproj`/`.slnx` files.
- New files in your crate must be registered in
  `Microservices/Microservices.projitems` — but that file is shared surface,
  so include the exact `<None Include=… />` line in your **handoff note** for
  the orchestrator instead of editing it yourself.
- Work on your team branch `team/<service-name>/<task>`. Never merge or
  rebase another team's branch.

## Contract discipline

- Your service's inputs and outputs are defined by the contract types in
  `messaging-core` (messages) and `db-core` (schema) plus your HTTP API.
  You **consume** these; you never change them.
- Need a new field, subject, endpoint shape, or table change? File a
  **contract change request** to the Orchestration Agent: what you need, why,
  proposed shape, whether additive or breaking. Then keep working against the
  current contract until the orchestrator ships the new version.

## Testing obligations (all three, every task)

1. **Unit tests — complete.** Every public function and every branch of
   business logic, runnable with `cargo test -p <your-service>` and zero
   external dependencies (no NATS, no Postgres, no sibling service).

   There are **no traits to mock** in the core crates — `Messaging`,
   `IdempotencyGuard` and `db-core` are concrete. Do not add trait
   abstractions to make something testable; that is shared surface and not
   yours to change. Instead **split the decision from the I/O** and test the
   decision: a pure function taking plain values, called by a thin async
   wrapper that does the network work. `worker-service::process` and
   `mimic-service::classify_service` are the pattern to copy.
2. **Contract tests — both directions.**
   - *Provider side:* every message/response your service emits is asserted
     to serialize exactly to the contract shape (round-trip through the
     `messaging-core` envelope; golden JSON where useful).
   - *Consumer side:* every message/request you accept is deserialized from
     contract-shaped fixtures, including unknown-field tolerance and the
     previous contract version if one is still live.
   - Contract tests are the stand-in for your neighbors: green contract tests
     mean you can ship without ever running a sibling service.

   **Where they go.** `src/contract_tests.rs`, declared in `main.rs` as
   `#[cfg(test)] mod contract_tests;`. *Not* `tests/` — every service here is
   a binary-only crate, and Rust's `tests/` directory can only import a
   crate's **lib** target. There isn't one, so `use gateway_service::…` will
   not compile. A module inside `src/` can reach private items through
   `use crate::…`, which is what these tests need.

   **Tool crates.** `mimic-service` and `dlq-replay` publish no messages to
   siblings, so "provider and consumer" reads oddly for them. They still owe
   contract tests, against the contract they actually have: `mimic-service`
   provides `/api/state` to its own page and consumes Prometheus and NATS
   JSON; `dlq-replay` consumes dead letters and provides command envelopes.
   `echo-service` has an HTTP contract rather than a messaging one.
3. **E2E is not yours.** Cross-service behavior belongs to the Orchestration
   Agent's e2e suite. Do not write tests that start sibling services.

## Definition of done

- `cargo test -p <your-service>` green, `cargo fmt --all --check` clean,
  `cargo clippy -p <your-service> --all-targets --all-features -- -D warnings`
  clean.
- Beginner-annotated Rust per CLAUDE.md: comments name the language concepts
  (`?`, ownership, traits, lifetimes); public items get `///` doc comments.
- Handoff note to the orchestrator: files added/removed (with `.projitems`
  lines), contract change requests, anything touching shared surface.
