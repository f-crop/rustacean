# Port Map — mars (100.87.157.74)

This document is the authoritative reference for all ports on the `mars` host.

**Rule:** Never bind a new service to a port already listed here. If a port is taken, pick a free one and update this table.

---

## Rustbrain Dev Stack (compose/dev.yml)

Deployed via:
```bash
docker compose --env-file compose/tailscale.env -f compose/dev.yml -f compose/tailscale.yml up -d
```

| Host Port | Dev Default | Service | Protocol | Notes |
|-----------|-------------|---------|----------|-------|
| 15432 | 5432 | postgres | TCP | PostgreSQL — rustbrain DB |
| 17474 | 7474 | neo4j | HTTP | Neo4j browser UI |
| 17687 | 7687 | neo4j | Bolt | Neo4j bolt driver |
| 16333 | 6333 | qdrant | HTTP | Qdrant REST API |
| 16334 | 6334 | qdrant | gRPC | Qdrant gRPC |
| 19094 | 9094 | kafka | TCP | Kafka external listener (KRaft) |
| 14317 | 4317 | otel-collector | gRPC | OTLP gRPC receiver |
| 14318 | 4318 | otel-collector | HTTP | OTLP HTTP receiver |
| 13200 | 3200 | tempo | HTTP | Grafana Tempo |
| 19090 | 9090 | prometheus | HTTP | Prometheus scrape + query |
| 19091 | 9091 | pushgateway | HTTP | Prometheus Pushgateway (full.yml) |
| 13000 | 3000 | grafana | HTTP | Grafana dashboards |
| 21434 | 11434 | ollama | HTTP | Ollama LLM API |
| 18080 | 8080 | control-api | HTTP | Rustbrain control plane API |
| 15173 | 15173 | frontend | HTTP | Vite dev server (React UI) — same on dev and tailscale |
| 10080 | 80 | caddy | HTTP | Reverse proxy HTTP |
| 10443 | 443 | caddy | HTTPS | Reverse proxy HTTPS |
| 18081 | 8081 | pgweb | HTTP | pgweb DB browser (read-only) |
| 18082 | 8082 | kafka-ui | HTTP | Kafka UI |
| 18888 | 8888 | hindsight | HTTP | Hindsight memory API + MCP server (`/mcp/rustacean/`) |
| 19999 | 9999 | hindsight | HTTP | Hindsight Control Plane UI |
| ${CLAUDE_SSH_HOST_PORT:-12222} | 22 | claude-login | SSH | Claude OAuth login sidecar — shares `claude-credentials` volume with agent-runner |

### Quick access via Tailscale

| Service | URL |
|---------|-----|
| Control API | http://100.87.157.74:18080 |
| Frontend (Vite dev) | http://100.87.157.74:15173 |
| Grafana | http://100.87.157.74:13000 |
| Prometheus | http://100.87.157.74:19090 |
| pgweb | http://100.87.157.74:18081 |
| Kafka UI | http://100.87.157.74:18082 |
| Neo4j browser | http://100.87.157.74:17474 |
| Hindsight API/MCP | http://100.87.157.74:18888 |
| Hindsight Control Plane | http://100.87.157.74:19999 |

---

## Existing mars Containers (pre-existing, do not reuse)

| Host Port | Container | Notes |
|-----------|-----------|-------|
| 443 | vcs | Gitea HTTPS |
| 2424 | vcs | Gitea SSH |
| 3001 | rustbrain-mcp-sse | MCP SSE |
| 3010 | mars-grafana | Mars monitoring Grafana |
| 3100 | paperclip | Paperclip server |
| 3103 | extensions-server | Governance extensions |
| 4096 | rustbrain-opencode | OpenCode |
| 5173 | paperclip-governance-board | Paperclip governance board plugin (do not reuse — frontend remapped to 15173) |
| 5432 | rustbrain-postgres | Shared Postgres (pre-existing stack) |
| 6333–6334 | rustbrain-qdrant | Qdrant (pre-existing stack) |
| 7474, 7687 | rustbrain-neo4j | Neo4j (pre-existing stack) |
| 8000 | portainer | Container management |
| 8081 | vcs | Gitea HTTP |
| 8085 | rustbrain-pgweb | pgweb (remapped from 8081) |
| 8088 | rustbrain-api | Control API (remapped from 8080) |
| 8092 | rustbrain-playground-ui | Playground |
| 8096 | mars-alert-webhook | Mars alerting |
| 9000, 9443 | portainer | Container management HTTPS |
| 9090 | rustbrain-prometheus | Prometheus (pre-existing stack) |
| 9098 | mars-alertmanager | Mars alertmanager |
| 9099 | mars-prometheus | Mars Prometheus |
| 9100 | rustbrain-node-exporter | Node exporter |
| 9115 | rustbrain-blackbox-exporter | Blackbox exporter |
| 9400 | mars-nvidia-dcgm-exporter | GPU monitoring |
| 11434 | rustbrain-ollama | Ollama LLM (pre-existing stack) |
| 54329 | paperclip-pg | Paperclip embedded PostgreSQL |

---

## Worker Service Metrics (internal network only)

Worker services (Kafka consumers) expose a Prometheus `/metrics` endpoint on port `9091` inside the Docker network. Prometheus scrapes them at `<service-name>:9091/metrics`. No host-port mapping is needed — Prometheus runs inside the same `rb-net` network.

| Container hostname | Internal port | Purpose |
|---|---|---|
| agent-runner | 9091 | Prometheus metrics |
| audit-worker | 9091 | Prometheus metrics |
| embed-worker | 9091 | Prometheus metrics |
| expand-worker | 9091 | Prometheus metrics |
| ingest-clone | 9091 | Prometheus metrics |
| ingest-graph | 9091 | Prometheus metrics |
| parse-worker | 9091 | Prometheus metrics |
| projector-neo4j | 9091 | Prometheus metrics |
| projector-pg | 9091 | Prometheus metrics |
| tombstoner | 9091 | Prometheus metrics |
| typecheck-worker | 9091 | Prometheus metrics |

Port is configurable via `RB_METRICS_PORT` env var (default `9091`).

---

## Notes

### Kafka external connectivity
When connecting to Kafka from outside the Docker network (e.g., from a laptop via Tailscale), use:
- Bootstrap server: `100.87.157.74:19094`

Kafka advertises `100.87.157.74:19094` to external clients (set via `KAFKA_ADVERTISED_HOST` in `tailscale.env`). Internal services within the compose network use `kafka:9092` (PLAINTEXT listener, unaffected).

### Pre-existing rustbrain-* containers
The containers listed under "pre-existing" with `rustbrain-*` names may be from a previous or parallel deployment. Before redeploying, confirm whether they are managed by this repo's compose files:
```bash
docker inspect <container-name> --format '{{index .Config.Labels "com.docker.compose.project.config_files"}}'
```
If they are from this repo, the new deployment will update them in-place (ports will shift to the 1XXXX range).

### Adding new services
1. Check both tables above for port conflicts.
2. Pick a port in the `1XXXX` range (or another free range).
3. Add an env-var default in `compose/dev.yml` and the remapped value in `compose/tailscale.env`.
4. Update this table.
