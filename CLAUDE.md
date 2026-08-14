# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## A note on how this file, and everything in this repository, is written

**Never use acronyms or abbreviations in prose.** Write the full name or term every time. A widely recognised acronym may appear in parentheses after the full name on **first mention only** — "North Atlantic Treaty Organization (NATO)" — and the full name is used everywhere after that.

This applies to documentation, code comments, doc comments, commit messages, and pull request descriptions alike. Identifiers are exempt, because they are names rather than prose: crate names such as `dlq-replay`, constants such as `ORDER_DLQ`, environment variables, command names, file names, and third-party product names all stay exactly as they are. When one of those appears in prose, gloss it once — "the dead-letter queue (`ORDER_DLQ`)".

## Current state

A Cargo workspace and a GitHub Actions continuous integration gate exist. The one piece of C# is `DevConsole/`, a launcher that exists so the Visual Studio F5 key starts the stack; it builds nothing the services use.

- `Cargo.toml` — virtual workspace manifest at the repository root, `members = ["Microservices/*", "e2e"]`. Dependency versions and lint levels live here; crates opt in with `dep.workspace = true` and `[lints] workspace = true`.

Shared libraries:

- `Microservices/service-core` — health probes, self-probe for container health checks, environment configuration, tracing and OpenTelemetry setup.
- `Microservices/messaging-core` — NATS and JetStream plumbing, message contracts, envelope, idempotency guard, trace propagation.
- `Microservices/db-core` — the PostgreSQL schema and migrations, shared so two services cannot fight over `_sqlx_migrations`.

Services:

- `Microservices/gateway-service` — front door. `POST /relay` calls `echo-service` synchronously; `POST /order` writes through a transactional outbox and relays to NATS.
- `Microservices/worker-service` — JetStream queue consumer (2 replicas). Retry, dead-lettering, idempotency.
- `Microservices/outbox-relay` — publishes committed outbox rows to NATS. Its own process, so request handling and relay throughput scale separately.
- `Microservices/notifier-service`, `Microservices/audit-service` — two independent subscribers to the same event.
- `Microservices/dlq-replay` — one-shot tool that puts dead letters back on the queue. Run with `DevReplay.cmd`, never automatically.
- `Microservices/echo-service` — the original synchronous Hypertext Transfer Protocol (HTTP) example. Copy it to start a simple service.

- `Microservices/mimic-service` — the console at `http://localhost:8090`: one page framing the live mimic panel, the walkthrough, Grafana, Jaeger and Prometheus, with a status strip driven by Prometheus and the NATS monitoring endpoint. `DevConsole.cmd` opens it. Shows *shape* — which component is amber and what sits downstream — which a row of graphs cannot.
- `observability/` — Prometheus scrape configuration and Grafana provisioning. Dashboards live in version control, not in the Grafana database, so a panel change is reviewable in a pull request.
- `compose.yaml` and the `Dev*.cmd` scripts — the local stack: 6 services, NATS, Jaeger, Prometheus, Grafana, PostgreSQL, Redis. Grafana on port 3000, Jaeger on port 16686. `DevDemo.cmd` narrates the whole demonstration. See `.claude/skills/dev-environment/SKILL.md` and `.claude/skills/messaging-and-eventing/SKILL.md`.
- `.github/workflows/ci.yml`, `.github/actions/setup-rust` and `.github/scripts/affected-crates.sh` — the continuous integration gate. Rules and failure modes are documented in `.claude/skills/rust-ci-gate/SKILL.md`; read that before changing continuous integration.
- `Dockerfile` and `.github/workflows/image.yml` — one parameterised image build for every service, published to the GitHub Container Registry with a provenance attestation and a Trivy vulnerability scan. See `.claude/skills/rust-service-image/SKILL.md`.
- `.github/workflows/security.yml` — CodeQL (public repositories only), the zizmor workflow audit, and the gitleaks secret scan. Every third-party action is pinned to a commit hash; see `.claude/skills/gh-supply-chain/SKILL.md` before adding one.
- `rust-toolchain.toml`, `clippy.toml`, `deny.toml`, `.config/nextest.toml` — tool configuration, all at the workspace root.
- `DemoRustMonoMicroservice.slnx` — Visual Studio solution, in the Extensible Markup Language `.slnx` format rather than the older `.sln` format. Holds a "Solution Items" folder for the root-level files plus the `Microservices` project.
- `Microservices/Microservices.shproj` and its `.projitems` — a C# shared project (`HasSharedItems`, root namespace `Microservices`) used only to surface the Rust files in Solution Explorer.
- `DevConsole/DevConsole.csproj` — the solution's only buildable project, and therefore its default startup project. A .NET 10 console application that walks up from its own location to find the repository root and runs a `Dev*.cmd` script through `cmd.exe`, forwarding the exit code. **Pressing F5 in Visual Studio starts the whole compose stack.** See "The one exception: `DevConsole/`" below.
- `hrdrClaudeNative.cmd`, `RunClaude.cmd` — agent launcher scripts (see below).

## Commands

Run from the repository root. `cargo` is not on the executable search path on this machine; it is at `~/.cargo/bin`.

```
cargo test --workspace --all-features
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo run -p echo-service            # PORT=8080 by default

docker build --build-arg SERVICE=echo-service -t echo-service:local .
docker run --rm -p 8080:8080 echo-service:local

DevStart.cmd     # whole stack: 5 services, NATS, Jaeger, PostgreSQL, Redis
DevDemo.cmd      # run and narrate the whole demonstration
DevReplay.cmd    # put dead letters back on the queue (--dry-run to look first)
DevConsole.cmd   # open every graphical interface in one page; starts the stack if it is down
DevStatus.cmd    # what is running, and is it healthy
DevLogs.cmd      # follow logs
DevStop.cmd      # stop, keep everything
DevDelete.cmd    # remove containers, keep images and data
DevRemove.cmd    # remove everything, including database volumes

dotnet run --project DevConsole            # same as DevConsole.cmd - this is what F5 runs
dotnet run --project DevConsole -- status  # any script by name; arguments after -- pass straight through
```

Continuous integration runs the same commands, plus `cargo nextest run --profile ci`, `cargo llvm-cov`, and `cargo deny check`.

## Team standard operating procedure — who may change what

Work in this repository is divided between **one Microservice Agent Team per service crate** and **one overwatch Orchestration Agent**. The two roles have different writable scope, and the split is the point: teams move fast inside a boundary, the orchestrator owns everything that crosses one.

| | Microservice Agent Team | Orchestration Agent |
| --- | --- | --- |
| Owns | exactly one crate under `Microservices/` | everything shared |
| May edit | its own `src/`, `tests/`, `Cargo.toml`, its migrations | platform crates, workspace root, compose, continuous integration, `.projitems`, `docs/`, `observability/`, `e2e/` |
| Contracts | **consumes** them, never changes them | **sole owner** — arbitrates and versions them |
| Tests | complete unit tests plus provider and consumer contract tests | end-to-end |
| Branch | `team/<service-name>/<task>` | merges; teams never merge each other |

Full definitions: `.claude/skills/microservice-agent-team/SKILL.md` and `.claude/skills/orchestration-agent/SKILL.md`. Load the one matching the role you are in.

### The three-layer pyramid

| Layer | Owner | Needs a running stack? |
| --- | --- | --- |
| **Unit** — every branch of business logic | the team | no |
| **Contract** — provider round-trip plus consumer fixtures | the team | no |
| **End-to-end** — cross-service choreography | the orchestrator | yes |

The middle layer is what lets a team ship without ever starting a sibling service: contract tests stand in for the neighbours. Consumer-side tests must tolerate **unknown fields**, because an additive contract change reaches producers before consumers and must not break them in between.

End-to-end tests are `#[ignore]` by default so `cargo test` stays green on a machine with nothing running. Run them with `cargo test -p e2e -- --ignored` after `DevStart.cmd`.

### Contract changes never happen peer-to-peer

A team needing a new field files a request with the orchestrator, keeps working against the current contract, and gets the new version pushed to producer and consumer in the same cycle. Changes are additive by default; a breaking one increments the version, migrates the producer first, then consumers, then retires the old version.

Two shared files a team may not edit but will need changed: `Microservices/Microservices.projitems` (to register a new `.rs` file) and anything under the "Orchestration Agent" column above. Put the exact line needed in the handoff note rather than editing it.

## Rust code is written for a Rust beginner

The owner of this repository is new to Rust. **Every `.rs` file and every `Cargo.toml` must explain the language concepts it uses**, not just the business logic: ownership and borrowing (`&`, `.clone()`, `move`), `Result` and `Option` and `?`, traits and `#[derive(...)]`, `async` and `await`, attribute macros such as `#[tokio::main]`, lifetimes such as `&'static str`, and why a field is a `String` rather than a `&str`.

Name the concept so it can be looked up later ("this is the *turbofish*", "`?` returns early with the error"). Prefer doc comments (`///`, `//!`) on public items so `cargo doc --open` produces a usable manual. This overrides the usual "match the surrounding comment density" instinct — here the language itself is the unfamiliar part, so idiomatic code that an experienced Rust reader would find obvious still needs a comment.

## Where Rust code goes

**All Rust microservices live under `Microservices/`** — crates in there, never sibling top-level directories.

**The one deliberate exception is `e2e/`**, the orchestrator-owned end-to-end crate. It is a test harness, not a service: it ships in no image and has no binary target. Because a top-level directory is not matched by `members = ["Microservices/*"]`, it is listed explicitly in the workspace, and because the `.projitems` table routes by location it gets its own viewer project. Both are required — miss either and the crate silently drops out of the build or out of Solution Explorer.

The solution is six shared projects plus one plain folder. **Every new file must be registered in the matching `.projitems`**, or it is invisible in Visual Studio even though it is committed and working:

| New file lives in | Register it in |
| --- | --- |
| `Microservices/…` | `Microservices/Microservices.projitems` |
| Cargo configuration at the repository root | `Cargo/Cargo.projitems` |
| `compose.yaml`, `Dockerfile`, `Dev*.cmd` | `DevEnvironment/DevEnvironment.projitems` |
| `.github/…` | `GitHub/GitHub.projitems` |
| `.claude/skills/…` | `AgentSkills/AgentSkills.projitems` |
| `e2e/…` (the end-to-end crate) | `e2e/E2E.projitems` |
| Loose repository metadata (`README.md`, `.gitignore`, launcher scripts) | `DemoRustMonoMicroservice.slnx`, in the `Solution Items` folder |
| `DevConsole/…` (the F5 launcher) | **nothing** — it is a real project that globs its own files |

`AgentSkills/`, `Cargo/`, `DevEnvironment/` and `GitHub/` contain **only** a `.shproj` and a `.projitems` — no real files. They link upward with `$(MSBuildThisFileDirectory)..\…` and a `Link` attribute controlling the displayed name. The files themselves cannot move: cargo, rustup, Docker, GitHub Actions, and Claude Code each require their own fixed location. These projects are viewers, exactly like `Microservices.shproj`.

Adding a file is still one line:

```xml
<None Include="$(MSBuildThisFileDirectory)..\newfile.toml" Link="newfile.toml" />
```

**Every `.rs` file must be registered in `Microservices/Microservices.projitems`** as an inert `<None Include="…" />` item. A shared project only surfaces files listed in `.projitems`, so an unregistered file is invisible in Solution Explorer. Adding or removing a Rust file is a two-step operation: change the file on disk *and* update `.projitems` in the same change. `<None>` items are never fed to the C# compiler, so this is display-only and cannot break a build.

Visual Studio is a **viewer** for this code, not its build system — the `.shproj` produces no build output and the C# code-sharing targets do nothing with a `.rs` file. Build, test, and lint with cargo from the command line; do not route those through Visual Studio. Do not restructure or delete a `.shproj` or `.projitems` for tidiness — they are the only thing making the folder openable in Visual Studio.

### The one exception: `DevConsole/`

`DevConsole/DevConsole.csproj` is the **only buildable project in the solution** — a small .NET 10 console application whose entire job is to find the repository root and run a `Dev*.cmd` script. Because every other project is a `.shproj` that cannot be started, Visual Studio picks this one as the startup project on its own: clone, open the solution, press **F5**, and the compose stack comes up.

Consequences worth knowing before touching it:

- It is a project in the modern software development kit style, so it **globs its own sources**. Do not add its files to any `.projitems` — that is the one place the "register every new file" rule does not apply.
- It contains no orchestration logic and must not grow any. The `Dev*.cmd` scripts are the single definition of what starting the stack means; a second copy here would drift from them.
- `Properties/launchSettings.json` gives the F5 dropdown a profile per script (Dev Start, Dev Status, Dev Logs, and so on). Adding a script means a line in the allow-list in `Program.cs` and a profile here.
- `DevConsole/` is listed in `.dockerignore`. No image needs it, and its `bin` and `obj` directories would otherwise be sent to the daemon on all eight image builds.
- Nothing in continuous integration builds it; the gate is cargo only.

## Agent launchers

`RunClaude.cmd` is the single place where the Claude Code invocation lives:

```
claude --dangerously-skip-permissions --verbose [--model %CLAUDE_MODEL%]
```

It runs `pushd` to the repository root first, so it works from any working directory. Changes to the Claude launch flags belong here, not duplicated elsewhere.

`hrdrClaudeNative.cmd` wraps `RunClaude.cmd` to launch Claude as a tracked, named pane inside the `herdr` terminal multiplexer. It is self-healing: it locates `herdr` on the executable search path or at `%LOCALAPPDATA%\Programs\Herdr\bin\herdr.exe`, installs or updates the native Claude integration hook for herdr, starts a herdr server if none is running, creates (or reuses) a workspace labelled after the repository directory, picks a free agent name (`claude`, `claude-2`, and so on), runs `RunClaude.cmd` in the new pane, then renames the detected agent and attaches this console.

Model selection flows from `hrdrClaudeNative.cmd` to the pane environment variable `CLAUDE_MODEL`, then to `RunClaude.cmd`, then to `--model`. The default is `claude-opus-5`; override it with `setx CLAUDE_MODEL "claude-sonnet-5"`.

When editing these scripts, note the constraints already worked around: herdr agent names must match `[a-z][a-z0-9_-]{0,31}` and be unique among live agents; herdr replies in JavaScript Object Notation, so lookups are delegated to an inline `powershell -NoProfile` call that reads `HERDR`, `REPO_DIR` and `WS_LABEL` from the environment to avoid nested quoting; `pane run` commands are wrapped in `cmd /c` because panes may run PowerShell.

## Repository conventions

- `.gitignore` is the standard GitHub Visual Studio template — it does **not** cover the Rust `target/` directory. `data/cipher-sessions.db*` is runtime state from the Cipher Model Context Protocol server and should not be committed; extend `.gitignore` when adding Rust or keeping local data.
- `.gitattributes` sets `* text=auto`; all the merge-driver and diff sections are commented out.
