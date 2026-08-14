---
name: gh-supply-chain
description: >
  Supply-chain and security controls for this repository — actions pinned by
  commit hash, the Security workflow (CodeQL, the zizmor workflow audit,
  gitleaks), and the Trivy image scan in the Image workflow. Use when adding or
  updating a GitHub Action, when a security check fails, when asked why CodeQL
  is not producing alerts, or when deciding what changes if this repository
  becomes public.
---

# Supply chain

Four controls, split by whether they work on a private repository.

> **Write everything out in full.** No acronyms or abbreviations in prose. A
> widely recognised acronym may follow the full term in parentheses on first
> mention only. Identifiers — workflow names, flags, metric names — stay as
> they are.

| Control | Where | Works on this repository while private? |
| --- | --- | --- |
| Actions pinned by commit hash | every workflow | yes |
| zizmor workflow audit | `security.yml` | yes |
| gitleaks secret scan | `security.yml` | yes |
| Trivy image vulnerability scan | `image.yml` | yes |
| CodeQL (Rust and actions) | `security.yml` | **no** — gated on being public |
| cargo-deny advisories | `ci.yml` | yes (predates this skill) |

## Non-negotiable rules

1. **Every third-party action is pinned to a commit hash, with the tag as a
   trailing comment.**

   ```yaml
   - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7
   ```

   A tag is mutable: whoever controls the repository can repoint `v7` at any
   commit, and your workflow then runs it with your `GITHUB_TOKEN`. Dependabot
   updates the hash *and* the comment, so pinning costs nothing in
   maintenance.

   Local actions (`uses: ./.github/actions/setup-rust`) need no pin — they
   come from the checked-out commit.

   Resolve a tag to its commit hash with:

   ```
   gh api repos/actions/checkout/commits/v7 --jq .sha
   ```

2. **Never interpolate `${{ }}` inside a `run:` block.** Pass the value
   through `env:` and reference it as a shell variable. GitHub substitutes
   `${{ }}` textually *before* the shell sees the script, so a value
   containing shell metacharacters executes. zizmor fails the build on this,
   including for values that happen to be safe today.

   ```yaml
   - env:
       CRATE: ${{ matrix.crate }}
     run: cargo nextest run -p "$CRATE"
   ```

3. **Every `actions/checkout` sets `persist-credentials: false`.** By default
   checkout writes the `GITHUB_TOKEN` of the job into `.git/config`, where
   anything that uploads or packages the workspace carries it out with it —
   including a Docker build context. No job here pushes with git, so the
   credential is never needed. zizmor calls this `artipacked` and fails on it.

4. **CodeQL is gated, not deleted.**
   `if: github.event.repository.visibility == 'public'`. Code scanning needs
   GitHub Advanced Security, which user-owned private repositories cannot
   have. The job skips, and `Security OK` treats skipped as success. Make the
   repository public and it starts running with no edit here.

5. **Trivy scans the locally loaded image, not a pushed one.**
   `build-push-action` has `load: true`, so the image exists in the daemon on
   the runner even during a pull request, where nothing is pushed. Scanning a
   registry reference instead would mean the scan only ever ran *after*
   publishing.

6. **`ignore-unfixed: true` on Trivy is deliberate.** A vulnerability with no
   available patch is not actionable in a build gate; leaving those in
   produces a red check that everyone learns to ignore. Advisories against
   Rust crates are the job of cargo-deny, not Trivy.

## Adding a new action

1. Resolve the commit hash: `gh api repos/<owner>/<repo>/commits/<tag> --jq .sha`
2. Write `uses: owner/repo@<hash> # <tag>`
3. Nothing else — the `github-actions` ecosystem in Dependabot already covers
   the whole repository and groups all bumps into one pull request.

## What changes if this repository becomes public

Three things switch on by themselves, with no edits:

- CodeQL analysis for Rust and for actions, with Copilot Autofix on alerts
- Build provenance attestation in `image.yml`
- The GitHub secret scanning and push protection features

At that point gitleaks becomes a second line of defence rather than the only
cover.

## Debugging

- **zizmor fails on a workflow you just wrote**: almost always rule 2. Read
  the finding — it names the file and the line.
- **gitleaks flags a test fixture**: add a `.gitleaksignore` entry keyed by
  the fingerprint of the finding, with a comment saying why it is not a real
  secret. Never disable the job.
- **Trivy suddenly fails with no code change**: a new advisory landed against
  the base image. Increment `RUST_VERSION` in the Dockerfile, or the
  distroless tag, and rebuild.
- **CodeQL never reports anything**: check that the repository is public. On a
  private repository the job is skipped by design and reports as such in the
  run summary.

## `up` does not mean what it looks like

`up{service="X"} == 1` means **"something answered at the address the domain
name system gave me"** — not "X is alive". Docker recycles container network
addresses: delete a service and the next container to start can be handed the
address it used to hold. Prometheus keeps scraping it, gets a valid `/metrics`
response from an entirely different process, and reports the dead service as
healthy.

That is not hypothetical; it happened here, and it is why every service now
emits `service_info{service="…"}` naming itself. Anything asking "is this
service alive" — dashboards, alert rules, the mimic panel — must count
`service_info`, not `up`.

Use a bounded window too, or a vanished target reads as healthy for the
five-minute default lookback in Prometheus:

```
count(last_over_time(service_info{service="worker-service"}[30s]))
```

## Known gaps

1. `dependabot.yml` increments versions but nothing enforces a review window;
   a malicious release could be merged automatically if automatic merging is
   ever enabled.
2. No software bill of materials is produced or published.
   `docker/build-push-action` can emit one with `sbom: true`; it pairs
   naturally with the attestation once that works.
3. The base images in the Dockerfile are pinned by tag, not by digest. A
   digest pin is stricter but needs a manual bump, since the docker ecosystem
   in Dependabot is not configured here.
