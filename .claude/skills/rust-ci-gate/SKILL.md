---
name: rust-ci-gate
description: >
  Add, change, or debug the GitHub Actions CI gate for this Rust microservice
  monorepo — affected-crate detection, fmt/clippy/nextest/llvm-cov/cargo-deny
  jobs, the aggregate required check, merge-queue compatibility, and the
  composite setup action. Use whenever a new crate is added under
  Microservices/, when a CI job is added or reshaped, when a required status
  check is not reporting, or when the merge queue stalls.
---

# Rust CI gate

The gate is `.github/workflows/ci.yml`. Everything below is a rule that has a
failure mode behind it — change one and know which failure you are accepting.

## Layout

| File | Role |
| --- | --- |
| `.github/workflows/ci.yml` | The gate itself |
| `.github/actions/setup-rust/action.yml` | Composite: toolchain + cache + cargo tools |
| `.github/scripts/affected-crates.sh` | Diff → JSON array of crates to test |
| `rust-toolchain.toml` | Single source of the toolchain, local and CI |
| `clippy.toml`, `deny.toml`, `.config/nextest.toml` | Tool config, workspace root |

## Non-negotiable rules

1. **`ci-ok` is the only required status check.** Point branch protection /
   rulesets at `CI OK` and nothing else. Path-filtered jobs report `skipped`,
   and GitHub never receives a status for a job that did not run — a skipped
   required check leaves the PR pending forever and wedges the merge queue.
   `ci-ok` runs `if: always()` and fails unless every `needs` entry is
   `success` or `skipped`.

2. **Every required check triggers on `merge_group`.** A workflow that only
   lists `pull_request` produces no check run when a PR enters the queue, so
   the queue times the PR out and ejects it. `ci.yml` lists `pull_request`,
   `merge_group`, and `push` to `master`.

3. **`concurrency.cancel-in-progress` is `pull_request`-only.** Cancelling a
   merge-queue run reports a failed check and ejects the PR. The expression is
   `${{ github.event_name == 'pull_request' }}`.

4. **Affected-crate detection is PR-only.** On `merge_group` and `push` the
   matrix is the whole workspace, because those events validate the merged
   result. If the full suite becomes too slow there, change the fallback in the
   `affected` job — do not weaken the aggregate check.

5. **Any change to a shared build input tests everything.** `affected-crates.sh`
   short-circuits to the full crate list when the diff touches `Cargo.toml`,
   `Cargo.lock`, `rust-toolchain.toml`, `clippy.toml`, `deny.toml`, `.config/`,
   or `.github/`. Keep that list in sync with what actually affects builds.

## Adding a crate

1. Create it under `Microservices/<name>/` — the workspace globs
   `members = ["Microservices/*"]`, so no manifest edit is needed.
2. Add `[lints] workspace = true` to its `Cargo.toml`, and take dependency
   versions from `[workspace.dependencies]` with `dep.workspace = true`.
3. Register every `.rs` file in `Microservices/Microservices.projitems` as
   `<None Include="$(MSBuildThisFileDirectory)<name>\src\...rs" />`. Unregistered
   files are invisible in Visual Studio. See CLAUDE.md.
4. Nothing in `ci.yml` changes. The matrix is generated from `cargo metadata`.

## Cache keys

Jobs that build with different flags must pass different `cache-key` values to
`setup-rust`. The coverage job builds instrumented (`-C instrument-coverage`);
sharing the test job's cache entry makes both jobs rebuild every run. Current
keys: `metadata`, `lint`, `test`, `coverage`.

## Toolchain

`setup-rust` runs `rustup toolchain install` with no argument, which reads
`rust-toolchain.toml`. Never name a version in the workflow — that is the drift
this avoids. `rustfmt` and `clippy` come from the toolchain file's `components`;
`llvm-tools-preview` is added per-job because only coverage needs it.

## Known gaps, in priority order

1. Actions are pinned by tag, not commit SHA. Pin by digest (Dependabot then
   maintains them) as part of the `gh-supply-chain` skill.
2. Coverage threshold is `--fail-under-lines 60`. Raise it as the workspace
   grows; lowering it needs a reason in the PR body.
3. `affected-crates.sh` is bash + jq, so the `affected` job must stay on a
   Linux runner. It runs under `set -euo pipefail`; when adding to it, remember
   that expanding `${#array[@]}` on a declared-but-empty associative array is an
   *unbound variable* error, not a zero. Guard with `${array[*]+x}` first — this
   already bit the docs-only-change path once.

## Debugging

- **Check stuck in "Expected — waiting for status"**: a required check name does
  not match a job that ran. Confirm only `CI OK` is required.
- **Merge queue ejects every PR**: the workflow is missing `merge_group`, or a
  run was cancelled by `concurrency`.
- **Matrix job fails with `fromJSON` error**: `affected-crates.sh` printed
  something other than a JSON array. It echoes `Affected: …` in the job log.
- **Empty matrix**: `needs.affected.outputs.any` is `false`, `test` and
  `coverage` skip, and `ci-ok` still passes. That is intended for docs-only PRs.
