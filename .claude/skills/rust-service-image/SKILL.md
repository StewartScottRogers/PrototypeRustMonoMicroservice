---
name: rust-service-image
description: >
  Build, publish, and attest container images for the Rust services in this
  monorepo — the shared cargo-chef Dockerfile, the GHCR push, and SLSA build
  provenance. Use when adding a service that needs an image, changing the
  Dockerfile or its base images, debugging a slow or cache-missing image build,
  a GHCR push that is denied, or a failing attestation.
---

# Rust service images

One `Dockerfile` at the repo root builds every service. `.github/workflows/image.yml`
publishes them to GHCR with a signed provenance attestation.

## Layout

| File | Role |
| --- | --- |
| `Dockerfile` | Parameterised by `--build-arg SERVICE=<crate>` |
| `.dockerignore` | Keeps `target/` out of the build context |
| `.github/workflows/image.yml` | Discover → matrix build → push → attest |
| `.github/scripts/binary-crates.sh` | Workspace crates with a `bin` target |

Build one locally:

```
docker build --build-arg SERVICE=echo-service -t echo-service:local .
docker run --rm -p 8080:8080 echo-service:local
```

## Non-negotiable rules

1. **Never interpolate `github.repository` into an image name.** GHCR rejects
   uppercase characters and this repo is `StewartScottRogers/PrototypeRustMonoMicroservice`.
   The `Compute image name` step lowercases it. A direct interpolation fails at
   push time with a name-invalid error, after the whole build has run.

2. **Attestation needs three permissions, not one.** `packages: write` (push the
   attestation beside the image), `id-token: write` (the OIDC token is what
   proves the build ran in this repo and workflow), `attestations: write`
   (GitHub's attestation store). Missing `id-token: write` is the usual cause of
   a signing failure.

3. **`provenance: false` on `build-push-action` is deliberate.** BuildKit's own
   provenance attachment and `actions/attest-build-provenance` both write to the
   image index; running both produces a confusing double attestation. Keep the
   GitHub one — it is what `gh attestation verify` reads.

4. **PRs build but must not push.** `push: ${{ github.event_name != 'pull_request' }}`.
   A PR from a fork has a read-only `GITHUB_TOKEN` and would fail the push
   anyway; more importantly an unreviewed PR must not publish a tag.

5. **`cargo chef cook` runs before the sources are copied.** Inverting those two
   layers means every source edit recompiles all dependencies — the entire
   reason cargo-chef exists. If build times jump, check that order first.

6. **The dependency layer is cooked workspace-wide**, not per-service. Every
   service image then shares one cached layer. Adding `-p <service>` to the cook
   step makes each image compile its own copy of the same crates.

## Adding a service

Nothing to change here. `binary-crates.sh` reads `cargo metadata`, so any new
crate with a `bin` target gets an image on the next run. Library crates like
`service-core` are excluded automatically.

## Base images

- Builder: `rust:${RUST_VERSION}-slim-bookworm`. `RUSTUP_TOOLCHAIN` is pinned to
  the same version because `rust-toolchain.toml` asks for `stable`, and without
  the override rustup downloads a second identical toolchain inside the image.
- Runtime: `gcr.io/distroless/cc-debian12:nonroot`. No shell, no package
  manager, runs as uid 65532. Services must listen above port 1024 — `EXPOSE 8080`
  and `PORT` default to 8080 for that reason.

Debugging a distroless image has no `docker exec … sh`. Rebuild with
`--target builder` and exec into that instead.

## Verifying an attestation

```
gh attestation verify oci://ghcr.io/stewartscottrogers/prototyperustmonomicroservice/echo-service:latest \
  --repo StewartScottRogers/PrototypeRustMonoMicroservice
```

This is SLSA v1.0 Build Level 2. Level 3 needs a hardened, isolated builder;
GitHub-hosted runners do not qualify on their own.

## Known gaps, in priority order

1. **sccache is not wired in.** cargo-chef caches the dependency layer as a
   unit; sccache would cache individual compilation units, so a single changed
   dependency stops invalidating all of them. It needs `ACTIONS_RUNTIME_TOKEN`
   forwarded into the BuildKit build, which is fragile — do it only if
   dependency rebuild time becomes the bottleneck.
2. Images are amd64 only. Add `platforms: linux/amd64,linux/arm64` to
   `build-push-action` plus a cross-compilation step; QEMU emulation for a Rust
   build is slow enough to be a poor default.
3. Actions are tag-pinned, not SHA-pinned — same gap as the CI gate, same fix in
   the `gh-supply-chain` skill.
4. No image is scanned for CVEs. That belongs with `gh-supply-chain` too.
