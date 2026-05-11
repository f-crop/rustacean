# syntax=docker/dockerfile:1

# ── Stage 1: build ────────────────────────────────────────────────────────────
FROM rust:1-bookworm AS builder

WORKDIR /app

# Cache dependency downloads before copying source.
COPY Cargo.toml Cargo.lock ./
COPY crates/rb-tracing/Cargo.toml      crates/rb-tracing/Cargo.toml
COPY crates/rb-schemas/Cargo.toml      crates/rb-schemas/Cargo.toml
COPY crates/rb-kafka/Cargo.toml        crates/rb-kafka/Cargo.toml
COPY services/migrate/Cargo.toml       services/migrate/Cargo.toml
COPY services/control-api/Cargo.toml   services/control-api/Cargo.toml
COPY services/audit-worker/Cargo.toml  services/audit-worker/Cargo.toml
COPY services/ingest-clone/Cargo.toml    services/ingest-clone/Cargo.toml
COPY services/expand-worker/Cargo.toml   services/expand-worker/Cargo.toml
COPY services/parse-worker/Cargo.toml    services/parse-worker/Cargo.toml
COPY services/typecheck-worker/Cargo.toml services/typecheck-worker/Cargo.toml
COPY services/ingest-graph/Cargo.toml    services/ingest-graph/Cargo.toml
COPY services/projector-pg/Cargo.toml    services/projector-pg/Cargo.toml
COPY services/projector-neo4j/Cargo.toml services/projector-neo4j/Cargo.toml
COPY services/tombstoner/Cargo.toml      services/tombstoner/Cargo.toml
COPY services/embed-worker/Cargo.toml    services/embed-worker/Cargo.toml
COPY services/agent-runner/Cargo.toml    services/agent-runner/Cargo.toml

# Stub sources so `cargo build` caches deps without the full source tree.
RUN find crates services -name "Cargo.toml" | sed 's|Cargo.toml|src/lib.rs|' | \
    xargs -I{} sh -c 'mkdir -p "$(dirname {})" && touch "{}"' && \
    mkdir -p services/agent-runner/src && \
    printf 'fn main() {}' > services/agent-runner/src/main.rs && \
    cargo build --release -p agent-runner 2>/dev/null || true && \
    rm -rf crates/*/src services/*/src

# Install build dependencies (rdkafka requires cmake + C toolchain).
RUN apt-get update -qq && \
    apt-get install -y --no-install-recommends \
        cmake libssl-dev libsasl2-dev zlib1g-dev libcurl4-openssl-dev pkg-config \
        gcc g++ && \
    rm -rf /var/lib/apt/lists/*

# Copy real source and build.
COPY crates/                 crates/
COPY services/agent-runner/  services/agent-runner/
COPY proto/                  proto/

RUN cargo build --release -p agent-runner

# ── Stage 2: minimal runtime image ────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

# Install git and ssh for cloning during agent execution
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
