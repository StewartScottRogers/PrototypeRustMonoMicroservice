---
name: rust-release
description: >
  Cut releases of the Rust crates in this monorepo with release-plz — raising a
  version by hand, the per-crate git tags and GitHub Releases that follow, and
  the versioned container image built from each tag. Use when a release did not
  happen, when a version number moved unexpectedly, when adding a crate that
  should or should not be released, or when wondering why there is no automatic
  release pull request.
---

# Releasing the Rust crates

`release-plz.toml` and `.github/workflows/release.yml` turn a raised version
number into a git tag, a GitHub Release, and a versioned container image.
Nothing is published to any registry.

> **Write everything out in full.** No acronyms or abbreviations in prose. A
> widely recognised acronym may follow the full term in parentheses on first
> mention only. Identifiers — crate names, tags, environment variables — stay as
> they are.

## The loop, once through

1. Raise the version in the crate's own `Cargo.toml` — `0.1.0` to `0.2.0` — in
   the ordinary pull request that earns the raise. **This is the release
   decision**, and it is a human one.
2. That pull request is reviewed and merged like any other.
3. The `Release` workflow runs `release-plz release`. It compares each
   manifest version against the tags that exist, finds `worker-service` at
   `0.2.0` with no tag, and creates `worker-service-v0.2.0` plus a GitHub
   Release. Crates whose version already has a tag are left alone, so the
   workflow is safe to run repeatedly.
4. That tag matches the `*-v*` trigger in `.github/workflows/image.yml`, which
   builds **only** `worker-service` and publishes it as `:0.2.0` and `:latest`.

## Why there is no release pull request

release-plz can open a pull request that bumps versions and writes changelogs
from Conventional Commits. **It cannot work here, and this is settled rather
than outstanding.**

To decide the next version it runs `cargo package`, which resolves every
dependency from a registry. Every service here depends on `messaging-core`,
`db-core` or `service-core`, and those exist nowhere but this repository. Both
possible `publish` settings were tried against both commands, and the two
requirements are mutually exclusive:

| Root manifest | `release-pr` | `release` |
| --- | --- | --- |
| `publish = false` | fails — `cargo package` cannot resolve the registry | **skips every crate**, tags included |
| `publish = ["internal"]` | fails — same | works: tags all eleven crates |

So the job was removed rather than left to fail on every push. What would bring
it back is a registry holding the three library crates — crates.io, or a
private registry — at which point `cargo package` resolves and the automation
becomes available. That is a real decision about publishing internal libraries,
not a workaround somebody forgot to apply.

Conventional Commit subjects are still worth writing: they are what a changelog
would be generated from if that day comes, and they read better in the history
regardless. They simply do not move a version number today.

## Non-negotiable rules

1. **Tagging with `GITHUB_TOKEN` builds no image.** A tag pushed using the
   automatic token starts no other workflow — GitHub's loop-prevention rule —
   so `image.yml` never sees it and no versioned image is ever built. The tags
   and the GitHub Releases still appear, which is why this looks like it worked
   until somebody goes looking for the image. `RELEASE_PLZ_TOKEN` is a
   fine-grained personal access token that is not subject to that rule. Set it
   once, from the repository's **Settings → Secrets and variables → Actions**
   page, scoped to this repository with **Contents: read and write**.

   Not with `gh secret set`, unless you are at a real terminal. That command
   **prompts** for the value, so anywhere its input is piped or redirected — a
   script, a tool, the `!` prefix in a Claude Code session — it reads
   end-of-input, stores an *empty* secret, and exits successfully with no
   output. The secret then exists, lists normally with a creation timestamp,
   and behaves exactly as though it were absent.

   Without the token, tags and releases still happen; the image builds have to
   be dispatched by hand, one per tag:

   ```
   gh workflow run image.yml --ref worker-service-v0.2.0
   ```

2. **Each crate owns its version number.** No crate inherits
   `version.workspace = true`; the root `[workspace.package]` has no `version`
   key at all. This is what lets `worker-service` reach `0.2.0` while
   `echo-service` stays at `0.1.0`. Adding `version.workspace = true` back to a
   crate silently ties it to whatever the workspace says and defeats the whole
   arrangement.

3. **The tag shape is load-bearing.** `git_tag_name` is
   `{{ package }}-v{{ version }}`, and the discover job in `image.yml` splits
   the crate name and the version back out of it. Changing the shape in
   `release-plz.toml` without changing that script produces tags that build
   nothing.

4. **`type=semver` cannot be used on these tags.** `worker-service-v0.2.0` is
   not a valid version string, so `docker/metadata-action` skips it with a
   warning rather than failing. The version comes from the discover job output
   instead.

5. **`publish = false` in a manifest stops releases dead — do not put it back.**
   The root `Cargo.toml` says `publish = ["internal"]`, and no registry called
   `internal` is configured anywhere, so `cargo publish` refuses with
   `registry index was not found in any configuration`. The obvious spelling
   is `publish = false`, and that is what it was until the first release ran
   and produced `nothing to release` with no tags at all: release-plz skips a
   package that cargo marks unpublishable *entirely*, tags included. The
   named-registry form is refused by cargo just as firmly while leaving the
   package visible to release-plz. `publish = false` in `release-plz.toml` is
   the separate switch that stops the pipeline running `cargo publish`.

   The named registry has one knock-on effect, already handled: cargo-deny
   stops treating these crates as private, and `allow-wildcard-paths` only
   exempts private crates. The fix is not to weaken that check — the three
   internal libraries in `[workspace.dependencies]` now carry a version
   requirement alongside their path, so there is no wildcard left to allow.
   That is the honest declaration anyway once crates version independently, and
   release-plz rewrites those numbers itself when a library is bumped. Setting
   `[licenses.private] registries` in `deny.toml` does *not* work: the bans
   check does not consult it.

   `git_only = true` belongs to the same fix. Without it release-plz asks
   crates.io which version is current, and for a crate that was never
   published the answer is nothing, so it concludes there is nothing to do.
   `git_only` points that question at the git tags instead, which is where the
   answer lives for this workspace.

6. **cargo-semver-checks is off, deliberately.** It compares a crate's public
   application programming interface against the last version *published to a
   registry*. Nothing here is published, so there is no baseline and the check
   can only mislead. The contract tests in each crate are what guard
   compatibility between services; see `.claude/skills/orchestration-agent/`.

7. **`dependencies_update = false`.** Dependabot owns dependency changes, and
   mixing a `cargo update` into a release would make a version bump
   indistinguishable from a dependency change in review.

8. **Never test a secret in a job-level condition.** The `secrets` context is
   not available there. Using it does not fail that line — it invalidates the
   **whole workflow file**, and every job in it fails to start, including the
   one that creates the tags. GitHub reports only "This run likely failed
   because of a workflow file issue". Ask the question in a *step*, and pass
   the answer out as a job output if a job needs it.

9. **One job, and it must not overlap itself.** `concurrency: release` with
   `cancel-in-progress: false`: two runs would race to create the same tag, and
   cancelling half-way through tagging would leave some crates released and
   others not.

## Adding a crate

Nothing to change here — release-plz reads `cargo metadata`, so a new crate is
picked up automatically. Give it a literal `version = "0.1.0"` rather than
`version.workspace = true`, per rule 2, and leave `publish.workspace = true`
alone so it inherits the named-registry guard, per rule 5.

To keep a crate out of releases, add a block to `release-plz.toml`:

```toml
[[package]]
name = "some-harness"
release = false
changelog_update = false
git_tag_enable = false
git_release_enable = false
```

`e2e` is already excluded this way. It is a test harness, ships in no image, and
nothing depends on it, so a version number for it would mean nothing.

## Failure modes

| Symptom | Cause | Fix |
| --- | --- | --- |
| A secret **exists** and a workflow still cannot see it | The stored value is empty. An empty secret and a missing secret are indistinguishable to a workflow, and `gh secret list` shows both the same way | Almost always `gh secret set` run without a real terminal attached: it prompts for the value, reads end-of-input immediately, stores an empty string, and exits silently and successfully. Anything that pipes or redirects its input does this — including the `!` prefix inside a Claude Code session. Set it from the repository's **Settings → Secrets and variables → Actions** page instead. |
| `This run likely failed because of a workflow file issue`, and **no job ran at all** | Something in the file references a context where GitHub does not allow it. The one that caused this: `secrets` in a job-level `if` | Rule 8. |
| `cargo package failed` in any release-plz command | `release-pr` or `update` was reinstated | It cannot work here; see "Why there is no release pull request". Raise the version by hand instead. |
| Raised a version, merged, no tag appeared | The `Release` workflow did not run, or ran before the merge commit landed | Re-run it on `master`. It is safe to run repeatedly and is a no-op when versions and tags already match. |
| Tag exists, no image published | Tag pushed with `GITHUB_TOKEN`, so no workflow triggered | Set `RELEASE_PLZ_TOKEN`. To publish the image now, run `Image` from `workflow_dispatch` on that tag. |
| Release job logs `nothing to release` and creates no tags | A manifest went back to `publish = false`, or `git_only = true` was removed from `release-plz.toml` | Rule 5. Confirm with `release-plz release --dry-run --backend github --git-token "$(gh auth token)"`, which lists every tag it would create. |
| `Image` builds nothing on a tag | The tag names a library crate or `e2e` | Correct behaviour — those have no binary target. The discover job logs which crate it read out of the tag. |
| A library changed but its dependents were not re-released | Nothing bumps them for you now that versions are raised by hand | Raise the dependents too, in the same pull request as the library. Keep shared-crate changes in their own pull request so that blast radius is visible in review. |

## Deliberately not done

- **No deployment.** Tags and images are the end of this pipeline. Promoting an
  image to production, the environment and its approval gate belong to the
  `gh-deploy-env` skill.
- **No release on a branch other than `master`.** Release candidates and
  maintenance branches would need `release_always` and a branch configuration in
  `release-plz.toml`; there is no second supported version of anything here yet.
- **No signed tags.** The provenance attestation on the image already ties a
  published artefact to the workflow and commit that built it, which is the
  claim that matters. See `.claude/skills/rust-service-image/SKILL.md`.
