# compose/dev.env.tpl — template for dev.env
#
# DO NOT commit dev.env (it is gitignored and may contain real secrets).
# Commit this template instead, then generate your local dev.env from it:
#
#   cp compose/dev.env.tpl compose/dev.env
#   # Edit dev.env to fill in secrets (GH App keys, etc.)
#
# Usage with docker compose:
#   docker compose --env-file compose/dev.env -f compose/dev.yml up -d
#
# See compose/env.schema.toml for descriptions and validators for each var.

# ─────────────────────────────────────────────────────────────────────────────
# Application — required
# ─────────────────────────────────────────────────────────────────────────────

# REQUIRED: User-facing frontend origin.
# WARNING: Must be the frontend URL (e.g. http://localhost:15173 or http://<tailscale-ip>:15173),
# NOT the API port (:8080). Wrong value breaks all email links and the GH install redirect.
RB_BASE_URL=http://localhost:15173

# ─────────────────────────────────────────────────────────────────────────────
# GitHub App (optional — GitHub routes return 503 when unset)
# ─────────────────────────────────────────────────────────────────────────────

# RB_GH_APP_ID=         # numeric GitHub App ID, e.g. 123456
# RB_GH_APP_PRIVATE_KEY=  # base64-encoded RSA PEM: base64 -w0 < app.pem
# RB_GH_APP_WEBHOOK_SECRET=dev-secret-do-not-use-in-prod
# RB_GH_APP_ENC_KEY=    # base64 of 32-byte AES-256 key for the Manifest flow; openssl rand 32 | base64 -w0

# For ingest-clone (raw PEM, not base64):
# GITHUB_APP_ID=
# GITHUB_APP_PRIVATE_KEY_PEM=-----BEGIN RSA PRIVATE KEY-----\n...\n-----END RSA PRIVATE KEY-----
# GITHUB_WEBHOOK_SECRET=dev-secret-do-not-use-in-prod

# ─────────────────────────────────────────────────────────────────────────────
# CORS (optional — defaults to localhost:15173)
# ─────────────────────────────────────────────────────────────────────────────

# RB_CORS_ORIGINS=http://localhost:15173

# ─────────────────────────────────────────────────────────────────────────────
# Host port remapping (optional — only needed when default ports conflict)
# ─────────────────────────────────────────────────────────────────────────────
# All of these default to the standard ports in compose/dev.yml.
# Uncomment and override below if another service on this machine is using a port.
#
# POSTGRES_HOST_PORT=5432
# NEO4J_HTTP_HOST_PORT=7474
# NEO4J_BOLT_HOST_PORT=7687
# QDRANT_REST_HOST_PORT=6333
# QDRANT_GRPC_HOST_PORT=6334
# KAFKA_HOST_PORT=9094
# KAFKA_ADVERTISED_HOST=localhost
# OTEL_GRPC_HOST_PORT=4317
# OTEL_HTTP_HOST_PORT=4318
# TEMPO_HOST_PORT=3200
# PROMETHEUS_HOST_PORT=9090
# GRAFANA_HOST_PORT=3000
# LOKI_HOST_PORT=3100
# OLLAMA_HOST_PORT=11434
# CONTROL_API_HOST_PORT=8080
# CADDY_HTTP_HOST_PORT=80
# CADDY_HTTPS_HOST_PORT=443
# FRONTEND_HOST_PORT=15173
# PGWEB_HOST_PORT=8081
# KAFKA_UI_HOST_PORT=8082
