# rust-brain v2

[![CI](https://github.com/jarnura/rustacean/actions/workflows/ci.yml/badge.svg)](https://github.com/jarnura/rustacean/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

**rust-brain** is a multi-tenant Git hosting and code-intelligence platform built in Rust. It ships a Rust monorepo backend, a React control-plane UI, and a streaming data pipeline to deliver fast, secure, and observable developer tooling.

## What it does

- **Multi-tenant Git hosting** — each tenant gets an isolated PostgreSQL schema; full repository hosting, code-search, and AI code intelligence are on the roadmap.
- **Auth & identity** — signup, email verification, password reset, sliding-window sessions, API keys, and multi-tenant switching — all production-hardened.
- **Control-plane API** — OpenAPI-first REST API (Axum 0.8 + utoipa 5) powering the frontend and future CLI clients.
- **Observability from day one** — OpenTelemetry traces, Prometheus metrics, Grafana Tempo, and Grafana dashboards ship in the default dev stack.

## Quick start

> Requirements: Docker ≥ 24 with Compose V2, Rust 1.85+, Node 20+.  
> Full instructions: [docs/getting-started.md](docs/getting-started.md)

```bash
# 1. Clone the repo
git clone https://github.com/jarnura/rustacean.git
cd rustacean

# 2. Start the full dev stack (infrastructure + control-api)
docker compose -f compose/dev.yml up -d

# 3. Run database migrations (first time only)
RB_DATABASE_URL=postgres://rustbrain:rustbrain@localhost:5432/rustbrain \
  cargo run -p migrate -- up

# 4. Start the frontend dev server
cd frontend && npm install && npm run dev
```

The API is live at `http://localhost:8080`. The frontend is at `http://localhost:15173`.  
Health check: `curl http://localhost:8080/health`

## Repository layout

```
rustacean/
├── crates/
│   ├── rb-auth/         # Password hashing, sessions, API-key generation
│   ├── rb-email/        # Email templates (Jinja2 via minijinja) + SMTP/console transports
│   ├── rb-github/       # GitHub App authentication (JWT, installation tokens) + API client
│   ├── rb-mcp/          # MCP JSON-RPC types and handler dispatch
│   ├── rb-schemas/      # Protobuf schema definitions (build.rs generated)
│   ├── rb-secrets/      # Zeroizing secret-value wrappers
│   ├── rb-storage-pg/   # PostgreSQL repository abstractions (sqlx)
│   ├── rb-tenant/       # Tenant context and schema-name derivation
│   └── rb-tracing/      # OpenTelemetry + tracing-subscriber initialisation
├── services/
│   ├── agent-runner/    # AI agent subprocess runner (Claude Code, OpenCode adapters)
│   ├── control-api/     # Main HTTP API service (Axum 0.8)
│   └── migrate/         # Database and Kafka migration runner
├── frontend/            # React 18 + Vite + TypeScript + Tailwind + shadcn/ui
├── compose/
│   ├── dev.yml          # Full dev stack: postgres, kafka, neo4j, qdrant, observability, API
│   ├── full.yml         # Full stack including all optional services
│   └── test.yml         # Minimal stack used by integration tests
├── docker/
│   └── control-api/     # Multi-stage Dockerfile for the API service
├── docs/
│   ├── getting-started.md
│   ├── architecture.md
│   ├── runbook.md
│   ├── runbooks/        # Operator runbooks per subsystem
│   ├── api-reference.md
│   ├── business-context.md
│   ├── PORT_MAP.md
│   ├── decisions/       # Architecture Decision Records (ADRs)
│   └── CODEMAPS/        # Per-service code maps
└── openapi.json         # Generated OpenAPI 3.1 spec — do not hand-edit
```

## Documentation

| Document | Description |
|----------|-------------|
| [Getting Started](docs/getting-started.md) | Prerequisites, clone, env setup, run, verify |
| [Architecture](docs/architecture.md) | System design, crate map, auth flow, schema-per-tenant |
| [Runbook](docs/runbook.md) | Start/stop, logs, health checks, migrations, failure modes |
| [API Reference](docs/api-reference.md) | Every endpoint with request/response examples |
| [Business Context](docs/business-context.md) | Problem statement, target users, product vision, roadmap |
| [Port Map](docs/PORT_MAP.md) | Port assignments for the mars host |
| [Operator Runbooks](docs/runbooks/) | Per-subsystem troubleshooting playbooks |
| [ADRs](docs/decisions/) | Architecture Decision Records (ADR-009 through ADR-011) |
| [Codemaps](docs/CODEMAPS/) | Per-service module trees and public API tables |

## Development commands

```bash
# Build the workspace
cargo build --workspace

# Run all tests
cargo test --workspace

# Lint and format
cargo fmt --check
cargo clippy --workspace -- -D warnings

# Regenerate OpenAPI spec (commit the result alongside handler changes)
cargo run -p control-api -- print-openapi > openapi.json

# Frontend: regenerate TypeScript types from the spec
cd frontend && npm run gen:api

# Frontend: typecheck
cd frontend && npm run typecheck
```

