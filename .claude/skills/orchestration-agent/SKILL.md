---
name: orchestration-agent
description: >
  Act as the overwatch Orchestration Agent for the monorepo: administer the
  Microservice Agent Teams, own and version the inter-service data contracts,
  and own the end-to-end test suite. Use when spinning up or assigning teams,
  when any change touches messaging-core, db-core, service-core, the workspace
  root (Cargo.toml, Cargo.lock, compose.yaml, .projitems), when two teams need
  a schema or contract change, or when cross-service (e2e) behavior must be
  verified. Never use this role to edit a service crate's internal code — that
  belongs to its team.
---

# Orchestration Agent

One overwatch agent sits above the Microservice Agent Teams. It has two jobs;
keep them mentally separate.

## Job 1 — Administration

- Assign exactly one team per service crate under `Microservices/`. A team's
  writable scope is its own crate directory and nothing else.
- Teams run **concurrently** as SOP. Give each team its own git branch (or
  worktree) named `team/<service-name>/<task>`. The orchestrator merges;
  teams never merge each other's branches.
- All shared surface is orchestrator-mediated. Teams may not edit:
  - `Microservices/service-core`, `messaging-core`, `db-core` (platform crates)
  - root `Cargo.toml`, `Cargo.lock`, `clippy.toml`, `deny.toml`,
    `rust-toolchain.toml`, `compose.yaml`, `Dockerfile`, `.dockerignore`,
    `Dev*.cmd`
  - `.github/**`, `.claude/skills/**`, `observability/**`, `docs/**`,
    the top-level `e2e/` crate, and any `.projitems` / `.shproj` / `.slnx`
  A team needing a change there files a request; the orchestrator makes the
  change (registering new files in the matching `.projitems` per CLAUDE.md)
  and notifies every affected team.
- Resolve collisions (ports, stream names, queue groups, migration ordering)
  before work starts, not at merge time.

## Job 2 — Contract arbitration

- The orchestrator is the **sole owner of inter-service data contracts**:
  message types, envelope, subjects/streams in `messaging-core`, and the
  Postgres schema + migrations in `db-core`.
- Teams never negotiate schema changes peer-to-peer. A consumer needing a new
  field requests it from the orchestrator; the orchestrator versions the
  contract, updates `messaging-core`/`db-core`, and pushes the change to both
  producer and consumer teams in the same cycle.
- Contract changes are **additive by default** (new optional field, new
  message version). A breaking change requires: bump the contract version,
  migrate producer first, consumers second, then retire the old version.
- Every contract change ships with updated provider and consumer contract
  tests (see the microservice-agent-team skill) before any team builds
  against it.

## Job 3 — End-to-end tests

- E2E tests verify cross-service choreography on the composed stack
  (`DevStart.cmd` world: gateway → outbox → NATS → worker/notifier/audit).
  No single team owns cross-service behavior, so e2e tests are orchestrator
  property.
- E2E lives outside any service crate, in the **top-level `e2e/` crate**.

  Two consequences follow, and both must be honoured or the crate silently
  falls out of the build and out of Visual Studio:

  1. The workspace glob is `members = ["Microservices/*"]`, which does **not**
     match a top-level directory. `e2e` is listed explicitly alongside it.
  2. CLAUDE.md's `.projitems` table routes by location, and no row covers the
     repo root. `e2e/` gets its **own viewer project** — `e2e/E2E.shproj` +
     `e2e/E2E.projitems`, sitting inside the folder they describe exactly as
     `Microservices.projitems` does, so their `Include` paths need no `..\`.
     (`Cargo/`, `DevEnvironment/`, `GitHub/` and `AgentSkills/` reach outwards
     instead, because they describe files at the repo root.)

  CLAUDE.md's "crates live under `Microservices/`, never sibling top-level
  directories" rule is about **services**. `e2e` is a test harness that ships
  in no image and is named as the one deliberate exception.

- E2E tests are `#[ignore]` by default. They need a composed stack, so an
  unqualified `cargo test` must skip them rather than fail on a machine with
  nothing running. Run them with `-- --ignored` after `DevStart.cmd`.
- The crate has a **lib target and no bin target**. The lib holds the harness
  (config, HTTP client, polling helpers) so `tests/` can import it; no bin
  means `binary-crates.sh` never builds a container image for it.
- Run order per merge cycle: each team's unit + contract tests gate their own
  PR (the existing affected-crates CI does this); the orchestrator runs e2e
  against the integrated branch before merging to main.
- `.github/scripts/smoke-test.sh` predates this crate and covers the same
  ground in bash from the CI `integration` job. They coexist deliberately: the
  script is the cheap gate that proves the stack came up, the crate is the
  richer suite with real assertions. Retiring the script is a decision to take
  once the crate covers everything it does — not a silent side effect.

## Beginner-annotated Rust still applies

Any Rust the orchestrator writes (contract types, e2e harness) follows the
CLAUDE.md rule: explain the language concepts in comments, doc-comment public
items.
