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
        cmake libssl-dev libsasl2-dev zlib1g-dev libcurl4-openssl-dev pkg-config && \
    rm -rf /var/lib/apt/lists/*

COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json -p tombstoner

COPY . .
RUN cargo build --release -p tombstoner

# ── Stage 4: minimal runtime image ────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

RUN apt-get update -qq && \
    apt-get install -y --no-install-recommends \
        ca-certificates libssl3 libsasl2-2 zlib1g libcurl4 && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd --gid 65532 nonroot && \
    useradd --uid 65532 --gid 65532 --no-create-home --system nonroot

USER nonroot

COPY --from=builder /app/target/release/tombstoner /usr/local/bin/tombstoner

ENTRYPOINT ["/usr/local/bin/tombstoner"]
