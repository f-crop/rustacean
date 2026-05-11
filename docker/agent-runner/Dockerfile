# syntax=docker/dockerfile:1

# ── Stage 1: chef base ────────────────────────────────────────────────────────
FROM lukemathwalker/cargo-chef:latest-rust-1-bookworm AS chef
WORKDIR /app

# ── Stage 2: planner ──────────────────────────────────────────────────────────
# Builds a recipe.json that captures the dependency graph from the full source
# tree. Only changes when Cargo.toml / Cargo.lock files change.
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ── Stage 3: builder ──────────────────────────────────────────────────────────
# Install apt build deps FIRST so they are cached independently of recipe.json.
# Then cook deps (cached unless the recipe changes), then copy source + build.
FROM chef AS builder

RUN apt-get update -qq && \
    apt-get install -y --no-install-recommends \
        cmake libssl-dev libsasl2-dev zlib1g-dev libcurl4-openssl-dev pkg-config \
        gcc g++ && \
    rm -rf /var/lib/apt/lists/*

COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json -p agent-runner

COPY . .
RUN cargo build --release -p agent-runner

# ── Stage 4: minimal runtime image ────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

# Install git and ssh for cloning during agent execution.
RUN apt-get update -qq && \
    apt-get install -y --no-install-recommends \
        ca-certificates libssl3 libsasl2-2 zlib1g libcurl4 git openssh-client && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd --gid 65532 nonroot && \
    useradd --uid 65532 --gid 65532 --no-create-home --system nonroot

# Pre-create the agent workspace mount point with nonroot ownership.
# Docker named volumes inherit ownership/permissions from the image's directory
# on first mount, so this ensures the runner (UID 65532) can write to
# /data/agent-workspaces without an init container or compose-level chown.
RUN mkdir -p /data/agent-workspaces && \
    chown 65532:65532 /data/agent-workspaces && \
    chmod 0700 /data/agent-workspaces

USER nonroot

COPY --from=builder /app/target/release/rb-agent-runner /usr/local/bin/rb-agent-runner

ENTRYPOINT ["/usr/local/bin/rb-agent-runner"]
