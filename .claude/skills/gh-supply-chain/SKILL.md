---
name: gh-supply-chain
description: >
  Supply-chain and security controls for this repo — SHA-pinned actions, the
  Security workflow (CodeQL, zizmor workflow audit, gitleaks), and the Trivy
  image scan in the Image workflow. Use when adding or updating a GitHub
  Action, when a security check fails, when asked why CodeQL is not producing
  alerts, or when deciding what changes if this repo becomes public.
---

# Supply chain

Four controls, split by whether they work on a private repository.

| Control | Where | Works on this private repo? |
| --- | --- | --- |
| SHA-pinned actions | every workflow | yes |
| zizmor workflow audit | `security.yml` | yes |
| gitleaks secret scan | `security.yml` | yes |
| Trivy image CVE scan | `image.yml` | yes |
| CodeQL (rust + actions) | `security.yml` | **no** — gated on public |
| cargo-deny advisories | `ci.yml` | yes (predates this skill) |

## Non-negotiable rules

1. **Every third-party action is pinned to a commit SHA, with the tag as a
   trailing comment.**

   ```yaml
   - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7
   ```

   A tag is mutable: whoever controls the repo can repoint `v7` at any commit,
   and your workflow runs it with your `GITHUB_TOKEN`. Dependabot updates the
   SHA *and* the comment, so pinning costs nothing in maintenance.

   Local actions (`uses: ./.github/actions/setup-rust`) need no pin — they come
   from the checked-out commit.

   Resolve a tag to its SHA with:

   ```
   gh api repos/actions/checkout/commits/v7 --jq .sha
   ```

2. **Never interpolate `${{ }}` inside a `run:` block.** Pass the value through
   `env:` and reference it as a shell variable. GitHub substitutes `${{ }}`
   textually *before* the shell sees the script, so a value containing shell
   metacharacters executes. zizmor fails the build on this, including for
   values that happen to be safe today.

   ```yaml
   - env:
       CRATE: ${{ matrix.crate }}
     run: cargo nextest run -p "$CRATE"
   ```

3. **Every `actions/checkout` sets `persist-credentials: false`.** By default
   checkout writes the job's `GITHUB_TOKEN` into `.git/config`, where anything
   that uploads or packages the workspace carries it out with it — including a
   Docker build context. No job here pushes with git, so the credential is
   never needed. zizmor calls this `artipacked` and fails on it.

4. **CodeQL is gated, not deleted.** `if: github.event.repository.visibility == 'public'`.
   Code scanning needs GitHub Advanced Security, which user-owned private
   repositories cannot have. The job skips, and `Security OK` treats skipped as
   success. Make the repo public and it starts running with no edit here.

5. **Trivy scans the locally loaded image, not a pushed one.** `build-push-action`
   has `load: true` so the image exists in the runner's daemon even on a pull
   request, where nothing is pushed. Scanning a registry reference instead would
   mean the scan only ever ran *after* publishing.

6. **`ignore-unfixed: true` on Trivy is deliberate.** A CVE with no available
   patch is not actionable in a build gate; leaving those in produces a red
   check that everyone learns to ignore. Advisories against Rust crates are
   cargo-deny's job, not Trivy's.

## Adding a new action

1. Resolve the SHA: `gh api repos/<owner>/<repo>/commits/<tag> --jq .sha`
2. Write `uses: owner/repo@<sha> # <tag>`
3. Nothing else — Dependabot's `github-actions` ecosystem already covers the
   whole repo and groups all bumps into one PR.

## What changes if this repo becomes public

Three things switch on by themselves, with no edits:

- CodeQL analysis for `rust` and `actions`, with Copilot Autofix on alerts
- Build provenance attestation in `image.yml`
- GitHub's own secret scanning and push protection

At that point gitleaks becomes belt-and-braces rather than the only cover.

## Debugging

- **zizmor fails on a workflow you just wrote**: almost always rule 2. Read the
  finding — it names the file and line.
- **gitleaks flags a test fixture**: add a `.gitleaksignore` entry keyed by the
  finding's fingerprint, with a comment saying why it is not a real secret.
  Never disable the job.
- **Trivy suddenly fails with no code change**: a new advisory landed against
  the base image. Bump `RUST_VERSION` in the Dockerfile, or the distroless tag,
  and rebuild.
- **CodeQL never reports anything**: check the repo is public. On a private
  repo the job is skipped by design and reports as such in the run summary.

## Known gaps

1. `dependabot.yml` bumps versions but nothing enforces a review window; a
   malicious release could be auto-merged if auto-merge is ever enabled.
2. No SBOM is produced or published. `docker/build-push-action` can emit one
   with `sbom: true`; it pairs naturally with the attestation once that works.
3. The Dockerfile's base images are pinned by tag, not digest. A digest pin is
   stricter but needs a manual bump, since Dependabot's docker ecosystem is not
   configured here.
