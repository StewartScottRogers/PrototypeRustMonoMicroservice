---
name: rust-release
description: >
  Cut releases of the Rust crates in this monorepo with release-plz — the
  Conventional Commits that drive version numbers, the release pull request, the
  per-crate git tags and GitHub Releases, and the versioned container image that
  follows from each tag. Use when a release did not happen, when a version
  number moved unexpectedly, when the release pull request has no status checks
  on it, when adding a crate that should or should not be released, or when
  deciding what a commit message needs to say.
---

# Releasing the Rust crates

`release-plz.toml` and `.github/workflows/release.yml` turn merged commits into
version numbers, changelogs, git tags, GitHub Releases, and versioned container
images. Nothing is published to crates.io — every crate sets `publish = false`.

> **Write everything out in full.** No acronyms or abbreviations in prose. A
> widely recognised acronym may follow the full term in parentheses on first
> mention only. Identifiers — crate names, tags, environment variables — stay as
> they are.

## The loop, once through

1. A pull request merges into `master` with a commit subject like
   `feat(worker-service): add a retry budget`.
2. The `Release` workflow runs `release-plz release-pr`. It sees a `feat`
   commit touching `Microservices/worker-service`, decides the next version is
   `0.2.0`, and opens one pull request titled `chore(release): ...` that edits
   `Microservices/worker-service/Cargo.toml` and writes
   `Microservices/worker-service/CHANGELOG.md`.
3. That pull request is reviewed like any other and merged. **This is the
   release decision.** Until it merges, nothing has been released.
4. The `Release` workflow runs again, this time reaching `release-plz release`.
   It compares each manifest version against the existing tags, finds
   `worker-service` at `0.2.0` with no tag, and creates the tag
   `worker-service-v0.2.0` plus a GitHub Release with the changelog section as
   its body.
5. That tag matches the `*-v*` trigger in `.github/workflows/image.yml`, which
   builds **only** `worker-service` and publishes it as `:0.2.0` and `:latest`.

## What decides the version number

Only the commit subject. release-plz reads Conventional Commits:

| Subject starts with | Changelog section | Version move |
| --- | --- | --- |
| `feat:` | Added | minor — `0.1.0` to `0.2.0` |
| `fix:` | Fixed | patch — `0.1.0` to `0.1.1` |
| `perf:` | Performance | patch |
| `refactor:` | Changed | patch |
| `doc:`, `test:`, `ci:`, `build:`, `chore:` | as named in `release-plz.toml` | patch |
| any of the above with `!`, or a `BREAKING CHANGE:` footer | flagged as breaking | major — `0.1.0` to `1.0.0` |

A subject matching none of these produces no changelog entry and no version
move. **A change worth releasing needs a conventional subject line**; there is
no way to force a release from an unconventional one except by editing the
version in the manifest by hand.

Scope the subject to the crate — `feat(worker-service): ...` — for a readable
changelog. The scope is documentation only; which crates get bumped is decided
from the paths the commit touched, not from the scope.

## Non-negotiable rules

1. **The release pull request needs a token that is not `GITHUB_TOKEN`.** A
   pull request opened using the automatic `GITHUB_TOKEN` triggers no other
   workflow — GitHub's loop-prevention rule. The ruleset on `master` requires
   `CI OK`, `Image OK` and `Security OK`, so such a pull request sits with no
   checks and can never be merged. The same rule means a tag pushed with
   `GITHUB_TOKEN` never starts the `Image` workflow, so no versioned image is
   ever built. Create the secret once:

   ```
   gh secret set RELEASE_PLZ_TOKEN
   ```

   Paste a fine-grained personal access token scoped to this repository with
   **Contents: read and write**, **Pull requests: read and write**, and
   **Workflows: read and write** (the last is needed only if a release ever
   touches a file under `.github/workflows/`).

   Until that secret exists the workflow falls back to `GITHUB_TOKEN` and still
   opens a release pull request — it simply has no checks. Closing and
   reopening it by hand starts them, which is the manual escape hatch, not the
   intended state.

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

5. **cargo-semver-checks is off, deliberately.** It compares a crate's public
   application programming interface against the last version *published to a
   registry*. Nothing here is published, so there is no baseline and the check
   can only mislead. The contract tests in each crate are what guard
   compatibility between services; see `.claude/skills/orchestration-agent/`.

6. **Dependency bumps stay out of the release pull request.**
   `dependencies_update = false`. Dependabot owns dependency changes, and a
   release pull request that also ran `cargo update` would make a version bump
   indistinguishable from a dependency change in review.

7. **The two jobs must not overlap.** `release` depends on `release-pr`, and the
   whole workflow uses `concurrency: release` with `cancel-in-progress: false`.
   Both write to the repository; cancelling mid-way through tagging would leave
   some crates released and others not.

## Adding a crate

Nothing to change here — release-plz reads `cargo metadata`, so a new crate is
picked up automatically. Give it a literal `version = "0.1.0"` rather than
`version.workspace = true`, per rule 2.

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
| Release pull request has no status checks | Opened with `GITHUB_TOKEN` | Set `RELEASE_PLZ_TOKEN` (rule 1). To unstick the existing one, close and reopen it. |
| Merged the release pull request, no tag appeared | The `release` job did not run, or ran before the merge commit landed | Re-run the `Release` workflow on `master`; it is safe to run repeatedly and is a no-op when versions and tags already match. |
| Tag exists, no image published | Tag pushed with `GITHUB_TOKEN`, so no workflow triggered | Set `RELEASE_PLZ_TOKEN`. To publish the image now, run `Image` from `workflow_dispatch` on that tag. |
| No release pull request at all after a merge | No commit since the last tag had a conventional subject | Land a commit with a `feat:` or `fix:` subject, or bump the version in the manifest by hand and let `release` pick it up. |
| One crate bumped, its dependents did not | Expected when only that crate's files changed | If a dependent must move too, that is a `fix(dependent): ...` commit against it. |
| Every crate bumped at once | A commit touched a shared crate under `Microservices/*-core`, so every dependent moved | Correct behaviour. Keep shared-crate changes in their own pull request so the blast radius is visible in review. |
| `Image` builds nothing on a tag | The tag names a library crate or `e2e` | Correct behaviour — those have no binary target. The discover job logs which crate it read out of the tag. |

## Deliberately not done

- **No deployment.** Tags and images are the end of this pipeline. Environments,
  protection rules and the Deployments application programming interface belong
  to the `gh-deploy-env` skill, which is the next one to build.
- **No release on a branch other than `master`.** Release candidates and
  maintenance branches would need `release_always` and a branch configuration in
  `release-plz.toml`; there is no second supported version of anything here yet.
- **No signed tags.** The provenance attestation on the image already ties a
  published artefact to the workflow and commit that built it, which is the
  claim that matters. See `.claude/skills/rust-service-image/SKILL.md`.
