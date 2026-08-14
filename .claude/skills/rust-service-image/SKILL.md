---
name: rust-service-image
description: >
  Build, publish, and attest container images for the Rust services in this
  monorepo — the shared cargo-chef Dockerfile, the push to the GitHub Container
  Registry, and Supply-chain Levels for Software Artifacts build provenance. Use
  when adding a service that needs an image, changing the Dockerfile or its base
  images, debugging a slow or cache-missing image build, a registry push that is
  denied, or a failing attestation.
---

# Rust service images

One `Dockerfile` at the repository root builds every service.
`.github/workflows/image.yml` publishes them to the GitHub Container Registry
with a signed provenance attestation.

> **Write everything out in full.** No acronyms or abbreviations in prose. A
> widely recognised acronym may follow the full term in parentheses on first
> mention only. Identifiers — image names, flags, environment variables — stay
> as they are.

## Layout

| File | Role |
| --- | --- |
| `Dockerfile` | Parameterised by `--build-arg SERVICE=<crate>` |
| `.dockerignore` | Keeps `target/` out of the build context |
| `.github/workflows/image.yml` | Discover, then matrix build, push, scan and attest |
| `.github/scripts/binary-crates.sh` | Workspace crates that have a binary target |

Build one locally:

```
docker build --build-arg SERVICE=echo-service -t echo-service:local .
docker run --rm -p 8080:8080 echo-service:local
```

## Non-negotiable rules

1. **Never interpolate `github.repository` into an image name.** The GitHub
   Container Registry rejects uppercase characters, and this repository is
   `StewartScottRogers/PrototypeRustMonoMicroservice`. The
   `Compute image name` step lowercases it. A direct interpolation fails at
   push time with a name-invalid error, after the whole build has run.

2. **Attestation needs three permissions, not one.** `packages: write` (to
   push the attestation beside the image), `id-token: write` (the OpenID
   Connect token is what proves the build ran in this repository and this
   workflow), and `attestations: write` (the GitHub attestation store).
   A missing `id-token: write` is the usual cause of a signing failure.

3. **Attestation only runs when the repository is public.** GitHub rejects
   `attest-build-provenance` on user-owned private repositories with "Feature
   not available for user-owned private repositories", *after* the image has
   already been pushed — so the job fails with a published image and no
   attestation. The step is gated on
   `github.event.repository.visibility == 'public'`. Make the repository
   public, or move it to an organisation on a Team or Enterprise plan, and it
   starts working with no edit here.

4. **`provenance: false` on `build-push-action` is deliberate.** The
   provenance attachment built into BuildKit and
   `actions/attest-build-provenance` both write to the image index; running
   both produces a confusing double attestation. Keep the GitHub one — it is
   what `gh attestation verify` reads.

5. **Pull requests build but must not push.**
   `push: ${{ github.event_name != 'pull_request' }}`. A pull request from a
   fork has a read-only `GITHUB_TOKEN` and would fail the push anyway; more
   importantly, an unreviewed pull request must not publish a tag.

6. **`cargo chef cook` runs before the sources are copied.** Inverting those
   two layers means every source edit recompiles all dependencies — the entire
   reason cargo-chef exists. If build times jump, check that order first.

7. **The dependency layer is cooked workspace-wide**, not per service. Every
   service image then shares one cached layer. Adding `-p <service>` to the
   cook step makes each image compile its own copy of the same crates.

8. **`ARG SERVICE` is declared *below* the cook step.** Declared above it, the
   argument becomes part of the cache key for every layer beneath, so each
   service rebuilds the entire dependency tree. This cost a 25-minute timeout
   once; moving the line brought the run back to 12 minutes.

## Adding a service

Nothing to change here. `binary-crates.sh` reads `cargo metadata`, so any new
crate with a binary target gets an image on the next run. Library crates such
as `service-core` are excluded automatically.

## Base images

- Builder: `rust:${RUST_VERSION}-slim-bookworm`. `RUSTUP_TOOLCHAIN` is pinned
  to the same version because `rust-toolchain.toml` asks for `stable`, and
  without the override rustup downloads a second identical toolchain inside
  the image.
- Runtime: `gcr.io/distroless/cc-debian12:nonroot`. No shell, no package
  manager, and it runs as user 65532. Services must listen above port 1024 —
  `EXPOSE 8080` and a `PORT` default of 8080 exist for that reason.

A distroless image has no shell, so `docker exec … sh` does not work. Rebuild
with `--target builder` and enter that instead.

## Verifying an attestation

```
gh attestation verify oci://ghcr.io/stewartscottrogers/prototyperustmonomicroservice/echo-service:latest \
  --repo StewartScottRogers/PrototypeRustMonoMicroservice
```

This is Supply-chain Levels for Software Artifacts version 1.0, Build Level 2.
Build Level 3 needs a hardened, isolated builder; runners hosted by GitHub do
not qualify on their own.

## Known gaps, in priority order

1. **sccache is not wired in.** cargo-chef caches the dependency layer as a
   unit; sccache would cache individual compilation units, so a single changed
   dependency stops invalidating all of them. It needs `ACTIONS_RUNTIME_TOKEN`
   forwarded into the BuildKit build, which is fragile — do it only if
   dependency rebuild time becomes the bottleneck.
2. Images are built for the 64-bit Intel and AMD architecture only. Add
   `platforms: linux/amd64,linux/arm64` to `build-push-action` plus a
   cross-compilation step; emulation of a Rust build under QEMU is slow enough
   to be a poor default.
3. Actions are pinned by tag rather than by commit hash — the same gap as the
   continuous integration gate, and the same fix, in the `gh-supply-chain`
   skill.

Vulnerability scanning is no longer a gap: `image.yml` runs the Trivy action
over each published image.
