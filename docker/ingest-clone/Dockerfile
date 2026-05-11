# syntax=docker/dockerfile:1

# ── Stage 1: build ────────────────────────────────────��───────────────────────
FROM rust:1-bookworm AS builder

WORKDIR /app

# Cache dependency downloads before copying source.
COPY Cargo.toml Cargo.lock ./
COPY crates/rb-tracing/Cargo.toml             crates/rb-tracing/Cargo.toml
COPY crates/rb-schemas/Cargo.toml             crates/rb-schemas/Cargo.toml
COPY crates/rb-kafka/Cargo.toml               crates/rb-kafka/Cargo.toml
COPY crates/rb-blob/Cargo.toml                crates/rb-blob/Cargo.toml
COPY crates/rb-github/Cargo.toml              crates/rb-github/Cargo.toml
COPY crates/rb-secrets/Cargo.toml             crates/rb-secrets/Cargo.toml
COPY services/migrate/Cargo.toml              services/migrate/Cargo.toml
COPY services/control-api/Cargo.toml          services/control-api/Cargo.toml
COPY services/audit-worker/Cargo.toml         services/audit-worker/Cargo.toml
COPY services/ingest-clone/Cargo.toml         services/ingest-clone/Cargo.toml
COPY services/expand-worker/Cargo.toml        services/expand-worker/Cargo.toml
COPY services/parse-worker/Cargo.toml         services/parse-worker/Cargo.toml
COPY services/typecheck-worker/Cargo.toml     services/typecheck-worker/Cargo.toml
COPY services/ingest-graph/Cargo.toml         services/ingest-graph/Cargo.toml
COPY services/projector-pg/Cargo.toml         services/projector-pg/Cargo.toml
COPY services/projector-neo4j/Cargo.toml      services/projector-neo4j/Cargo.toml
COPY services/tombstoner/Cargo.toml           services/tombstoner/Cargo.toml
COPY services/embed-worker/Cargo.toml         services/embed-worker/Cargo.toml
COPY services/agent-runner/Cargo.toml         services/agent-runner/Cargo.toml

# Stub sources so `cargo build` caches deps without the full source tree.
RUN find crates services -name "Cargo.toml" | sed 's|Cargo.toml|src/lib.rs|' | \
    xargs -I{} sh -c 'mkdir -p "$(dirname {})" && touch "{}"' && \
    mkdir -p services/ingest-clone/src && \
    printf 'fn main() {}' > services/ingest-clone/src/main.rs && \
    cargo build --release -p ingest-clone 2>/dev/null || true && \
    rm -rf crates/*/src services/*/src

# Install build dependencies (rdkafka requires cmake + C toolchain).
RUN apt-get update -qq && \
    apt-get install -y --no-install-recommends \
        cmake libssl-dev libsasl2-dev zlib1g-dev libcurl4-openssl-dev pkg-config \
        gcc g++ && \
    rm -rf /var/lib/apt/lists/*

# Copy real source and build.
COPY crates/                    crates/
COPY services/ingest-clone/     services/ingest-clone/
COPY proto/                     proto/

RUN cargo build --release -p ingest-clone

# ── Stage 2: minimal runtime image ────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

RUN apt-get update -qq && \
    apt-get install -y --no-install-recommends \
        ca-certificates libssl3 libsasl2-2 zlib1g libcurl4 git && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd --gid 65532 nonroot && \
    useradd --uid 65532 --gid 65532 --no-create-home --system nonroot

USER nonroot

COPY --from=builder /app/target/release/ingest-clone /usr/local/bin/ingest-clone

ENTRYPOINT ["/usr/local/bin/ingest-clone"]
