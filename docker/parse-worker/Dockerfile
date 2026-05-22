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
# parse-worker pulls in tree-sitter-rust grammar which needs cmake + C/C++.
FROM chef AS builder

RUN apt-get update -qq && \
    apt-get install -y --no-install-recommends \
        cmake libssl-dev libsasl2-dev zlib1g-dev libcurl4-openssl-dev pkg-config \
        gcc g++ && \
    rm -rf /var/lib/apt/lists/*

COPY --from=planner /app/recipe.json recipe.json
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo chef cook --release --recipe-path recipe.json -p parse-worker

COPY . .

# Declare AFTER the dep-cook step so that changing GIT_SHA only invalidates
# the final binary build, not the dependency cache layer.
ARG GIT_SHA=unknown
ARG BUILT_AT=unknown
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    RB_BUILD_SHA="${GIT_SHA}" RB_BUILD_TIMESTAMP="${BUILT_AT}" cargo build --release -p parse-worker && \
    cp target/release/parse-worker /usr/local/bin/

# ── Stage 4: minimal runtime image ────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

ARG GIT_SHA=unknown
LABEL git_sha=${GIT_SHA}
ENV RB_BUILD_SHA=${GIT_SHA}

RUN apt-get update -qq && \
    apt-get install -y --no-install-recommends \
        ca-certificates libssl3 libsasl2-2 zlib1g libcurl4 && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd --gid 65532 nonroot && \
    useradd --uid 65532 --gid 65532 --no-create-home --system nonroot

USER nonroot

COPY --from=builder /usr/local/bin/parse-worker /usr/local/bin/parse-worker

ENTRYPOINT ["/usr/local/bin/parse-worker"]
