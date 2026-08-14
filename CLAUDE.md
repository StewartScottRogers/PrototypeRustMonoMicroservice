# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Current state

A Cargo workspace and a GitHub Actions CI gate exist; there are no C# sources.

- `Cargo.toml` — virtual workspace manifest at the repo root, `members = ["Microservices/*"]`. Dependency versions and lint levels live here; crates opt in with `dep.workspace = true` and `[lints] workspace = true`.
Shared libraries:

- `Microservices/service-core` — health probes, self-probe for container healthchecks, env config, tracing + OpenTelemetry setup.
- `Microservices/messaging-core` — NATS/JetStream plumbing, message contracts, envelope, idempotency guard, trace propagation.
- `Microservices/db-core` — the Postgres schema and migrations, shared so two services cannot fight over `_sqlx_migrations`.

Services:

- `Microservices/gateway-service` — front door. `POST /relay` calls `echo-service` synchronously; `POST /order` writes through a transactional outbox and relays to NATS.
- `Microservices/worker-service` — JetStream queue consumer (2 replicas). Retry, dead-lettering, idempotency.
- `Microservices/outbox-relay` — publishes committed outbox rows to NATS. Its own process so HTTP and relay throughput scale separately.
- `Microservices/notifier-service`, `Microservices/audit-service` — two independent subscribers to the same event.
- `Microservices/dlq-replay` — one-shot tool that puts dead letters back on the queue. Run with `DevReplay.cmd`, never automatically.
- `Microservices/echo-service` — the original synchronous HTTP example. Copy it to start a simple service.

- `compose.yaml` + `Dev*.cmd` — the local stack: 5 services, NATS, Jaeger, Postgres, Redis. `DevDemo.cmd` narrates the whole demonstration. See `.claude/skills/dev-environment/SKILL.md` and `.claude/skills/messaging-and-eventing/SKILL.md`.
- `.github/workflows/ci.yml` + `.github/actions/setup-rust` + `.github/scripts/affected-crates.sh` — the CI gate. Rules and failure modes are documented in `.claude/skills/rust-ci-gate/SKILL.md`; read that before changing CI.
- `Dockerfile` + `.github/workflows/image.yml` — one parameterised image build for every service, published to GHCR with a provenance attestation and a Trivy CVE scan. See `.claude/skills/rust-service-image/SKILL.md`.
- `.github/workflows/security.yml` — CodeQL (public repos only), zizmor workflow audit, gitleaks secret scan. Every third-party action is pinned to a commit SHA; see `.claude/skills/gh-supply-chain/SKILL.md` before adding one.
- `rust-toolchain.toml`, `clippy.toml`, `deny.toml`, `.config/nextest.toml` — tool config, all at the workspace root.
- `DemoRustMonoMicroservice.slnx` — Visual Studio solution (XML `.slnx` format, not `.sln`). Holds a "Solution Items" folder for the root-level files plus the `Microservices` project.
- `Microservices/Microservices.shproj` + `.projitems` — a C# shared project (`HasSharedItems`, root namespace `Microservices`) used only to surface the Rust files in Solution Explorer.
- `hrdrClaudeNative.cmd`, `RunClaude.cmd` — agent launcher scripts (see below).

## Commands

Run from the repo root. `cargo` is not on PATH on this machine; it is at `~/.cargo/bin`.

```
cargo test --workspace --all-features
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo run -p echo-service            # PORT=8080 by default

docker build --build-arg SERVICE=echo-service -t echo-service:local .
docker run --rm -p 8080:8080 echo-service:local

DevStart.cmd     # whole stack: 5 services + NATS + Jaeger + Postgres + Redis
DevDemo.cmd      # run and narrate the whole demonstration
DevReplay.cmd    # put dead letters back on the queue (--dry-run to look first)
DevStatus.cmd    # what is running, and is it healthy
DevLogs.cmd      # follow logs
DevStop.cmd      # stop, keep everything
DevDelete.cmd    # remove containers, keep images and data
DevRemove.cmd    # remove everything, including database volumes
```

CI runs the same commands, plus `cargo nextest run --profile ci`, `cargo llvm-cov`, and `cargo deny check`.

## Rust code is written for a Rust beginner

The owner of this repo is new to Rust. **Every `.rs` file and `Cargo.toml` must explain the language concepts it uses**, not just the business logic: ownership and borrowing (`&`, `.clone()`, `move`), `Result`/`Option` and `?`, traits and `#[derive(...)]`, `async`/`await`, attribute macros like `#[tokio::main]`, lifetimes such as `&'static str`, and why a field is `String` rather than `&str`.

Name the concept so it can be looked up later ("this is the *turbofish*", "`?` returns early with the error"). Prefer doc comments (`///`, `//!`) on public items so `cargo doc --open` produces a usable manual. This overrides the usual "match the surrounding comment density" instinct — here the language itself is the unfamiliar part, so idiomatic code that an experienced Rust reader would find obvious still needs a comment.

## Where Rust code goes

**All Rust microservices live under `Microservices/`** — crates in there, never sibling top-level directories.

The solution is six shared projects plus one plain folder. **Every new file must be registered in the matching `.projitems`**, or it is invisible in Visual Studio even though it is committed and working:

| New file lives in | Register it in |
| --- | --- |
| `Microservices/…` | `Microservices/Microservices.projitems` |
| Cargo config at the repo root | `Cargo/Cargo.projitems` |
| `compose.yaml`, `Dockerfile`, `Dev*.cmd` | `DevEnvironment/DevEnvironment.projitems` |
| `.github/…` | `GitHub/GitHub.projitems` |
| `.claude/skills/…` | `AgentSkills/AgentSkills.projitems` |
| Loose repo metadata (`README.md`, `.gitignore`, launcher scripts) | `DemoRustMonoMicroservice.slnx`, in the `Solution Items` folder |

`AgentSkills/`, `Cargo/`, `DevEnvironment/` and `GitHub/` contain **only** a `.shproj` and a `.projitems` — no real files. They link upward with `$(MSBuildThisFileDirectory)..\…` and a `Link` attribute controlling the displayed name. The files themselves cannot move: cargo, rustup, Docker, GitHub Actions, and Claude Code each require their own fixed location. These projects are viewers, exactly like `Microservices.shproj`.

Adding a file is still one line:

```xml
<None Include="$(MSBuildThisFileDirectory)..\newfile.toml" Link="newfile.toml" />
```

**Every `.rs` file must be registered in `Microservices/Microservices.projitems`** as an inert `<None Include="…" />` item. A shared project only surfaces files listed in `.projitems`, so an unregistered file is invisible in Solution Explorer. Adding or removing a Rust file is a two-step operation: change the file on disk *and* update `.projitems` in the same change. `<None>` items are never fed to the C# compiler, so this is display-only and cannot break a build.

Visual Studio is a **viewer** for this code, not its build system — the `.shproj` produces no build output and the C# code-sharing targets do nothing with `.rs`. Build, test, and lint with cargo from the command line; don't route those through Visual Studio. Do not restructure or delete `.shproj` / `.projitems` for tidiness — they are the only thing making the folder openable in VS.

## Agent launchers

`RunClaude.cmd` is the single place where the Claude Code invocation lives:

```
claude --dangerously-skip-permissions --verbose [--model %CLAUDE_MODEL%]
```

It `pushd`es to the repo root first, so it works from any cwd. Changes to Claude's launch flags belong here, not duplicated elsewhere.

`hrdrClaudeNative.cmd` wraps `RunClaude.cmd` to launch Claude as a tracked, named pane inside the `herdr` terminal multiplexer. It is self-healing: locates `herdr` on PATH or at `%LOCALAPPDATA%\Programs\Herdr\bin\herdr.exe`, installs/updates herdr's native Claude integration hook, starts a herdr server if none is running, creates (or reuses) a workspace labelled after the repo directory, picks a free agent name (`claude`, `claude-2`, …), runs `RunClaude.cmd` in the new pane, then renames the detected agent and attaches this console.

Model selection flows `hrdrClaudeNative.cmd` → pane env (`CLAUDE_MODEL`) → `RunClaude.cmd` → `--model`. Default is `claude-opus-5`; override with `setx CLAUDE_MODEL "claude-sonnet-5"`.

When editing these scripts, note the constraints already worked around: herdr agent names must match `[a-z][a-z0-9_-]{0,31}` and be unique among live agents; herdr replies in JSON so lookups are delegated to an inline `powershell -NoProfile` call that reads `HERDR`/`REPO_DIR`/`WS_LABEL` from the environment to avoid nested quoting; `pane run` commands are wrapped in `cmd /c` because panes may run PowerShell.

## Repo conventions

- `.gitignore` is the standard GitHub VisualStudio template — it does **not** cover Rust (`target/`) directory. `data/cipher-sessions.db*` is runtime state from the Cipher MCP server and should not be committed; extend `.gitignore` when adding Rust or keeping local data.
- `.gitattributes` sets `* text=auto`; all the merge-driver and diff sections are commented out.
