# compose/ — Docker Compose stacks

## Which Postgres does the compose stack use?

**The compose stack runs its own Postgres** (`rustbrain-dev-postgres-1`, image `postgres:16-alpine`).
It does **not** connect to any pre-existing `rustbrain-postgres` container or the host Postgres on port 5432.

| Stack | DB host (inside compose network) | Host port |
|-------|----------------------------------|-----------|
| `dev.yml` (default) | `postgres:5432` | 5432 (or `$POSTGRES_HOST_PORT`) |
| `dev.yml` + `tailscale.env` (mars) | `postgres:5432` | 15432 |

Connection string used by `control-api` inside the compose network:
```
postgres://rustbrain:rustbrain@postgres:5432/rustbrain
```

## How migrations work

A `rb-migrations` init container runs **before** `control-api` starts:

1. Waits for the `postgres` service to be healthy.
2. Creates the `control` schema and a `control.schema_migrations` tracking table if they do not exist.
3. Applies each `.sql` file under `migrations/control/` in version order (idempotent — already-applied versions are skipped).
4. Exits 0 on success. `control-api` starts only after `rb-migrations` exits successfully
   (`depends_on: rb-migrations: condition: service_completed_successfully`).

The script is `compose/scripts/migrate-control.sh`. It mirrors the logic of the
`services/migrate` Rust binary so checksums match and future `migrate` invocations
recognise already-applied versions.

Adding a new control migration: drop a `.sql` file in `migrations/control/` with the next
version prefix (`003_...`). The next `docker compose up` will pick it up automatically.

## Start the stack

Use `scripts/dev-stack-up.sh` — it validates the env schema before starting and refuses to run without an explicit env file:

```bash
# 1. Create your env file from the template (first time only)
cp compose/dev.env.tpl compose/dev.env
# Edit compose/dev.env: set RB_BASE_URL to your frontend origin, fill in GH App keys if needed.

# 2. Start the stack
scripts/dev-stack-up.sh --env-file compose/dev.env -- -d

# mars / Tailscale (remapped ports)
scripts/dev-stack-up.sh --env-file compose/tailscale.env -- -d

# Infra only — skip control-api (e.g. running it with `cargo run`)
docker compose --env-file compose/dev.env -f compose/dev.yml up -d \
  postgres kafka neo4j qdrant otel-collector tempo prometheus grafana ollama
```

### Env schema validation

`compose/env.schema.toml` defines all env vars, their types, regex validators, and which services consume them. Run standalone:

```bash
# Validate any env file against the schema
compose/scripts/validate-env.sh compose/dev.env

# Validate only vars relevant to a specific service
compose/scripts/validate-env.sh compose/dev.env --service control-api
```

## Smoke test after `docker compose up -d control-api`

Run these to confirm the stack is healthy and migrations applied correctly.

```bash
# 1. Health check — expects {"status":"ok"}
PORT=${CONTROL_API_HOST_PORT:-8080}
curl -fsS http://localhost:${PORT}/health | jq .

# 2. Login endpoint — expects 401 (Unauthorized), NOT 500
#    A 401 proves the handler reached the DB and found no matching user.
#    A 500 with "relation does not exist" means migrations did not apply.
curl -s -o /dev/null -w "%{http_code}" \
  -X POST http://localhost:${PORT}/v1/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"email":"smoke@test.local","password":"irrelevant"}'
# → 401

# 3. Check logs for migration errors
docker logs rustbrain-dev-control-api-1 2>&1 | grep -i "relation\|error" | head -20
# → (no output expected)

# 4. Verify migration tracking table
docker compose -f compose/dev.yml exec postgres \
  psql -U rustbrain rustbrain \
  -c "SELECT version, description, applied_at FROM control.schema_migrations ORDER BY version;"
```

## Port reference

See `docs/PORT_MAP.md` for the full port table, including Tailscale remapping for mars.

## Auto-rebuild on main merge (mars)

The dev-stack on mars is kept current automatically. When a commit lands on `main` that touches a built service, `scripts/dev-stack-watch.sh` detects the new SHA and calls `scripts/dev-stack-auto-rebuild.sh`, which rebuilds only the affected images and restarts the containers.

**To skip one rebuild cycle** (e.g. during a planned manual intervention):
```bash
touch compose/.no-auto-rebuild   # on mars, before the merge
```
The file is consumed and deleted on the next polling cycle.

**To query recent rebuild results:**
```bash
scripts/dev-stack-auto-rebuild.sh --logs 10
```

Full documentation: [docs/dev-stack-auto-rebuild.md](../docs/dev-stack-auto-rebuild.md)

## Tear down

```bash
# Stop (preserves volumes — migrations stay applied)
docker compose -f compose/dev.yml down

# Full reset (drops volumes — migrations will re-run on next up)
docker compose -f compose/dev.yml down -v
```
