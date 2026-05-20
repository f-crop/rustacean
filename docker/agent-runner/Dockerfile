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
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo chef cook --release --recipe-path recipe.json -p agent-runner

COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release -p agent-runner && \
    cp target/release/rb-agent-runner /usr/local/bin/

# ── Stage 4: mcp-server-node builder ─────────────────────────────────────────
# Compile the TypeScript MCP bridge so the runtime image can install it
# globally as `rustbrain-mcp` — no npm-registry access needed at session time.
FROM node:20-bookworm-slim AS mcp-builder
WORKDIR /mcp
COPY packages/mcp-server-node/package.json packages/mcp-server-node/package-lock.json ./
RUN npm ci
COPY packages/mcp-server-node/src ./src
COPY packages/mcp-server-node/tsconfig.json ./
RUN npm run build

# ── Stage 5: runtime image ────────────────────────────────────────────────────
# Base on node:20-bookworm-slim so we have node/npm for:
#   • @anthropic-ai/claude-code (CLI binary the ClaudeCode adapter spawns)
#   • rustbrain-mcp (MCP bridge pre-installed from mcp-builder stage)
FROM node:20-bookworm-slim AS runtime

# Install git, ssh, and the C runtime deps the Rust binary links against.
# (Node base images already ship libssl3/zlib1g/libcurl4 but we install
# explicitly so the contract is obvious and survives base bumps.)
RUN apt-get update -qq && \
    apt-get install -y --no-install-recommends \
        ca-certificates libssl3 libsasl2-2 zlib1g libcurl4 git openssh-client && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd --gid 65532 nonroot && \
    useradd --uid 65532 --gid 65532 --no-create-home --system nonroot

# Install the claude CLI. The package ships a thin wrapper and pulls the
# correct native binary via the linux-x64 optionalDependency in postinstall.
# Pin to the version this image is tested against; bumps go through a separate
# chore PR. See the registry for the latest: https://registry.npmjs.org/@anthropic-ai/claude-code
ARG CLAUDE_CODE_VERSION=2.1.139
RUN npm install --omit=dev --no-fund --no-audit -g \
        @anthropic-ai/claude-code@${CLAUDE_CODE_VERSION} && \
    npm cache clean --force && \
    # Sanity-check that the postinstall replaced the placeholder shim with the
    # native binary. `claude --version` works without an API key, so we use it
    # as the canonical install probe — if the optionalDependency was skipped,
    # this fails the build instead of producing an image that silently can't
    # spawn claude at runtime.
    claude --version

# Install the opencode CLI. The opencode-ai package ships the `opencode` binary
# used by the OpenCode adapter (services/agent-runner/src/adapters/opencode.rs).
# Pin to the version this image is tested against; bumps go through a separate
# chore PR. See the registry for the latest: https://registry.npmjs.org/opencode-ai
ARG OPENCODE_VERSION=1.14.48
RUN npm install --omit=dev --no-fund --no-audit -g \
        opencode-ai@${OPENCODE_VERSION} && \
    npm cache clean --force && \
    opencode --version

# Pre-install the rust-brain MCP bridge from the compiled mcp-builder stage.
# This makes `rustbrain-mcp` available on PATH inside every spawned session
# without requiring npm-registry access or npx download at session start time.
COPY --from=mcp-builder /mcp/package.json /opt/rustbrain/mcp-server/package.json
COPY --from=mcp-builder /mcp/dist /opt/rustbrain/mcp-server/dist
RUN npm install -g /opt/rustbrain/mcp-server --omit=dev --no-fund --no-audit && \
    npm cache clean --force && \
    which rustbrain-mcp

# Pre-create the agent workspace mount point with nonroot ownership.
# Docker named volumes inherit ownership/permissions from the image's directory
# on first mount, so this ensures the runner (UID 65532) can write to
# /data/agent-workspaces without an init container or compose-level chown.
RUN mkdir -p /data/agent-workspaces && \
    chown 65532:65532 /data/agent-workspaces && \
    chmod 0700 /data/agent-workspaces

USER nonroot

COPY --from=builder /usr/local/bin/rb-agent-runner /usr/local/bin/rb-agent-runner

ENTRYPOINT ["/usr/local/bin/rb-agent-runner"]
