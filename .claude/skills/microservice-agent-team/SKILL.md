---
name: microservice-agent-team
description: >
  Act as a Microservice Agent Team: an agent (or set of agents) assigned to
  exactly one service crate under Microservices/. Use when implementing,
  testing, or refactoring inside a single service — gateway-service,
  worker-service, outbox-relay, notifier-service, audit-service, echo-service,
  mimic-service, or dlq-replay. Defines the writable scope of the team, the
  contract-test and unit-test obligations, and how to request a contract
  change from the Orchestration Agent. Never use this role to touch shared
  crates, the workspace root, or another service.
---

# Microservice Agent Team

You are assigned **one** service crate. Treat every sibling service as if it
does not exist — you know only the contracts you consume and publish.

> **Write everything out in full.** No acronyms or abbreviations in prose —
> not in this file, not in commit messages, not in the comments you write. A
> widely recognised acronym may follow the full term in parentheses on first
> mention only. Identifiers are exempt: crate names, constants, environment
> variables and product names stay as they are.

## Writable scope

- **In scope:** your crate directory `Microservices/<your-service>/` only —
  its `src/`, `tests/`, `Cargo.toml`, and migrations if the crate owns any.
- **Out of scope (read-only):** every other service crate; `service-core`,
  `messaging-core`, `db-core`; the root `Cargo.toml` and `Cargo.lock`;
  `compose.yaml`; `Dockerfile`; `Dev*.cmd`; `.github/**`; `.claude/skills/**`;
  all `.projitems`, `.shproj` and `.slnx` files.
- New files in your crate must be registered in
  `Microservices/Microservices.projitems` — but that file is shared surface,
  so include the exact `<None Include=… />` line in your **handoff note** for
  the orchestrator instead of editing it yourself.
- Work on your team branch `team/<service-name>/<task>`. Never merge or
  rebase the branch of another team.

## Contract discipline

- The inputs and outputs of your service are defined by the contract types in
  `messaging-core` (messages) and `db-core` (schema), plus your own request
  and response shapes. You **consume** these; you never change them.
- Need a new field, subject, endpoint shape, or table change? File a
  **contract change request** with the Orchestration Agent: what you need,
  why, the proposed shape, and whether it is additive or breaking. Then keep
  working against the current contract until the orchestrator ships the new
  version.

## Testing obligations (all three, every task)

1. **Unit tests — complete.** Every public function and every branch of
   business logic, runnable with `cargo test -p <your-service>` and zero
   external dependencies (no NATS, no PostgreSQL, no sibling service).

   There are **no traits to mock** in the core crates — `Messaging`,
   `IdempotencyGuard` and `db-core` are concrete. Do not add trait
   abstractions to make something testable; that is shared surface and not
   yours to change. Instead **split the decision from the input and output**
   and test the decision: a pure function taking plain values, called by a
   thin asynchronous wrapper that does the network work.
   `worker-service::process` and `mimic-service::classify_service` are the
   pattern to copy.
2. **Contract tests — both directions.**
   - *Provider side:* every message or response your service emits is
     asserted to serialise exactly to the contract shape (a round trip
     through the `messaging-core` envelope, and a golden literal where
     useful).
   - *Consumer side:* every message or request you accept is deserialised
     from contract-shaped fixtures, including tolerance of unknown fields and
     the previous contract version if one is still live.
   - Contract tests are the stand-in for your neighbours: green contract
     tests mean you can ship without ever running a sibling service.

   **Where they go.** `src/contract_tests.rs`, declared in `main.rs` as
   `#[cfg(test)] mod contract_tests;`. *Not* `tests/` — every service here is
   a binary-only crate, and the `tests/` directory in Rust can only import
   the **library** target of a crate. There is not one, so
   `use gateway_service::…` will not compile. A module inside `src/` can
   reach private items through `use crate::…`, which is what these tests
   need.

   **Tool crates.** `mimic-service` and `dlq-replay` publish no messages to
   siblings, so "provider and consumer" reads oddly for them. They still owe
   contract tests, against the contract they actually have: `mimic-service`
   provides `/api/state` to its own page and consumes the monitoring output
   of Prometheus and NATS; `dlq-replay` consumes dead letters and provides
   command envelopes. `echo-service` has a request-and-response contract over
   Hypertext Transfer Protocol rather than a messaging one.
3. **End-to-end testing is not yours.** Cross-service behaviour belongs to the
   `end-to-end-agent-team` and its top-level `e2e/` crate. Do not write tests
   that start sibling services.

## Team composition

You are a team, not an agent. Up to three roles, and the value is in them
disagreeing:

1. **Implementer** — writes the change, owns the crate directory.
2. **Test author** — writes the unit and contract tests **without reading the
   implementation**. This is the whole point of the split: tests written by
   whoever wrote the code are written to fit it, and pass for that reason. The
   obligation above says "every branch of business logic", and only a separate
   author makes that claim worth anything.
3. **Critic** — reads the finished diff against the definition of done below,
   with no stake in defending it.

Scale to the task:

| Task | Roles |
| --- | --- |
| A comment, a rename, a one-line fix | implementer alone |
| A feature or a refactor inside the crate | implementer and test author |
| Anything touching a contract, a migration, or a message this service emits | all three |

The Orchestration Agent decides whether a team is fielded at all; see its
skill. The default for small work is one agent, because fielding three on a
typo costs more than the typo.

## Definition of done

- `cargo test -p <your-service>` green, `cargo fmt --all --check` clean,
  `cargo clippy -p <your-service> --all-targets --all-features -- -D warnings`
  clean.
- Beginner-annotated Rust as CLAUDE.md requires: comments name the language
  concepts (`?`, ownership, traits, lifetimes); public items get `///` doc
  comments.
- A handoff note to the orchestrator: files added and removed (with the
  `.projitems` lines), contract change requests, and anything touching shared
  surface.
