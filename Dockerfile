# syntax=docker/dockerfile:1
#
# One Dockerfile for every service in the workspace. Pick one with --build-arg:
#
#   docker build --build-arg SERVICE=echo-service -t echo-service .
#
# The dependency layer is cooked workspace-wide on purpose: every service image
# then shares one cached layer instead of compiling the same crates per service.

ARG RUST_VERSION=1.97.0

FROM rust:${RUST_VERSION}-slim-bookworm AS chef
ARG RUST_VERSION
# rust-toolchain.toml asks for "stable"; the base image's toolchain is named by
# version. Without this rustup downloads a second, identical toolchain.
ENV RUSTUP_TOOLCHAIN=${RUST_VERSION} \
    CARGO_TERM_COLOR=always \
    CARGO_INCREMENTAL=0
WORKDIR /app
RUN cargo install cargo-chef --locked --version "^0.1"

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
ARG SERVICE
COPY --from=planner /app/recipe.json recipe.json
# Only recipe.json is present here, so this layer is reused until a dependency
# changes. Copying the sources first would rebuild every crate on every edit —
# that inversion is the whole point of cargo-chef.
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN test -n "${SERVICE}" || (echo "build arg SERVICE is required" >&2; exit 1) \
 && cargo build --release --locked -p "${SERVICE}" \
 && cp "target/release/${SERVICE}" /app/service

FROM gcr.io/distroless/cc-debian12:nonroot AS runtime
COPY --from=builder /app/service /usr/local/bin/service
# Ports below 1024 are unavailable to the nonroot user.
EXPOSE 8080
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/service"]
