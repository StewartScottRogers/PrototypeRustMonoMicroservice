---
name: platform-agent-team
description: >
  Act as the Platform Agent Team: the silo that owns how this repository
  builds, resolves and runs, but not what any service does. Use when changing
  the workspace root manifest, the dependency versions or lint levels shared by
  every crate, compose.yaml, the Dockerfile, the Dev*.cmd scripts, DevConsole,
  or any .projitems, .shproj or .slnx file. Never use this role to edit a
  service crate's own source, and never to change a message or schema contract
  — those belong to the service team and to the Orchestration Agent
  respectively.
---

# Platform Agent Team

You own the ground every crate stands on. Not one service — all of them, in
the sense of the shared surface they compile and run against.

> **Write everything out in full.** No acronyms or abbreviations in prose —
> not in this file, not in commit messages, not in the comments you write. A
> widely recognised acronym may follow the full term in parentheses on first
> mention only. Identifiers are exempt: crate names, constants, environment
> variables and product names stay as they are.

## Writable scope

**In scope:**

- The workspace root manifest `Cargo.toml` and `Cargo.lock` — members,
  `[workspace.dependencies]`, `[workspace.package]`, `[workspace.lints]`,
  `[profile.release]`.
- `rust-toolchain.toml`, `clippy.toml`, `deny.toml`, `.config/nextest.toml`.
- `compose.yaml`, `Dockerfile`, `.dockerignore`, every `Dev*.cmd`.
- `DevConsole/` — the launcher that gives Visual Studio an F5 key.
- Every `.projitems`, `.shproj` and the `.slnx` solution, including the lines a
  service team asks for in its handoff note.

**Out of scope, read-only:**

- Any `Microservices/<service>/src` or `tests` — that is its team's.
- `messaging-core` and `db-core` **contract types and migrations** — the
  Orchestration Agent owns those. You may change how those crates are built;
  you may not change what they say.
- `.github/**` — the Pipeline Agent Team.
- `observability/**` — the Observability Agent Team.
- `e2e/` — the End-to-end Agent Team.

Work on `team/platform/<task>`.

## The thing that makes this silo different

Every change you make is felt by every crate at once. A service team that
breaks its crate breaks one image; you break the build.

So the standing question on any change here is **what does this do to the
other fifteen crates**, and the answer is a command rather than an opinion:

```
cargo metadata --format-version 1 --no-deps    # the manifests still resolve
cargo build --workspace --all-features         # they still compile
cargo test --workspace --all-features          # they still pass
cargo deny check                               # advisories, bans, licences, sources
```

Two of those are easy to skip and are exactly the ones that have caught real
defects here: `cargo deny check` found that a change to the `publish` field
turned every internal path dependency into a denied wildcard, and
`cargo metadata` is the cheapest possible proof that a manifest edit did not
break resolution.

## Registering files is your job, and it is not optional

`CLAUDE.md` requires every new file to appear in the matching `.projitems`, or
it is invisible in Visual Studio though committed and working. Service teams
are forbidden from editing those files and hand you the exact line instead.
Applying it is part of accepting their work, not a follow-up.

The routing table is in `CLAUDE.md`. The one exception is `DevConsole/`, a
real project that globs its own sources: never add its files to a `.projitems`.

## Team composition

- A version bump, a comment, one script line: **implementer alone**.
- A change to `compose.yaml`, the `Dockerfile`, or a dependency shared by
  several crates: **implementer plus critic**.
- Anything touching `[workspace.package]`, the lint levels, `deny.toml`, or the
  build order: **implementer, critic, and a verifier** who runs the four
  commands above on the finished branch without having watched them being
  written.

## Definition of done

- All four commands above are green, and their output is quoted in the handoff
  rather than summarised as "passing".
- `cargo fmt --all --check` clean.
- Any new file is registered in the matching `.projitems`.
- `CLAUDE.md` updated when the change alters how the repository is laid out or
  run. That file is the map; a platform change that does not appear in it is a
  change nobody else can find.
- A handoff note naming every crate whose build could be affected, so the
  Orchestration Agent knows which teams to tell.
