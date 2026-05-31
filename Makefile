.PHONY: review-ready review-checklist review-checklist-fixtures state-reconcile-fixtures install-hooks blob-smoke blob-smoke-s3 ingest-smoke apply-branch-protection grafana-lint

# Run all local pre-PR checks: fmt, clippy, test, deny, openapi, frontend (if changed).
# All steps run even on partial failure so you see the full picture before pushing.
review-ready:
	bash scripts/review-ready.sh

# Run semantic reviewer checklist (mechanized catches that static analysis misses).
# PR Reviewer must paste the full output into their verdict comment.
# SKIP_DOCKER=1 to skip the image smoke; WAIVER_FILE=<path> to apply waivers.
review-checklist:
	bash scripts/review-checklist.sh

# Run fixture tests for the reviewer checklist (PASS and FAIL scenarios per check).
review-checklist-fixtures:
	bash tests/review-checklist/run-fixtures.sh

# Run fixture tests for the state-reconcile routine (AC-1 and AC-4 paths).
state-reconcile-fixtures:
	bash tests/state-reconcile/run-fixtures.sh

# Install git hooks (pre-push bundle detector). Safe to re-run.
install-hooks:
	bash scripts/install-hooks.sh

# Apply the 12 required status checks to the main branch protection (ADR-012 §S5.3).
# Idempotent — safe to run multiple times. Requires gh CLI with admin write access.
# Use --dry-run to preview without applying: make apply-branch-protection ARGS=--dry-run
apply-branch-protection:
	bash scripts/apply-branch-protection.sh $(ARGS)

# Validate that every Grafana dashboard panel only references metrics that are
# in the S2 frozen registry (ADR-012 §S3).  Fails if a panel queries a metric
# not listed in infra/grafana/metrics-registry.txt.
# Folded into ci/test per ADR-012 §S5.2.
grafana-lint:
	bash scripts/grafana-lint.sh


# Run SSE ingest smoke tests (no Kafka required — uses dev test-publish route).
#
# Requires a running control-api with RB_DEV_TEST_ROUTES=1 and a valid session
# cookie or bearer token in RB_SMOKE_TOKEN.  Defaults target localhost:3000.
#
# Usage:
#   make ingest-smoke
#   RB_SMOKE_BASE_URL=http://localhost:3001 RB_SMOKE_TOKEN=<jwt> make ingest-smoke
ingest-smoke:
	@echo "==> ingest smoke: rb-sse unit tests"
	cargo test -p rb-sse --features test-util -- --nocapture
	@echo "==> ingest smoke: test-producer binary compiles"
	cargo build -p control-api --bin rb-test-producer --quiet
	@echo "==> ingest smoke: PASS (SSE unit suite green; producer binary built)"
	@echo ""
	@echo "    To exercise the full SSE end-to-end path against a live server:"
	@echo "      1. Start control-api: RB_DEV_TEST_ROUTES=1 cargo run -p control-api"
	@echo "      2. Open an SSE stream: curl -N -H 'Authorization: Bearer \$$RB_SMOKE_TOKEN' \\"
	@echo "           \$${RB_SMOKE_BASE_URL:-http://localhost:3000}/v1/ingest/events"
	@echo "      3. Publish a test event: curl -X POST \\"
	@echo "           -H 'Authorization: Bearer \$$RB_SMOKE_TOKEN' \\"
	@echo "           -H 'Content-Type: application/json' \\"
	@echo "           -d '{\"event\":\"ingest.status\",\"data\":{\"status\":\"processing\"}}' \\"
	@echo "           \$${RB_SMOKE_BASE_URL:-http://localhost:3000}/v1/ingest/test-publish"

# Run filesystem blob store smoke tests (no external deps required).
blob-smoke:
	@echo "==> blob smoke: filesystem roundtrip"
	cargo test -p rb-blob fs_roundtrip -- --nocapture
	@echo "==> blob smoke: tenant isolation"
	cargo test -p rb-blob tenant_isolation -- --nocapture
	@echo "==> blob smoke: large blob (100 MiB)"
	cargo test -p rb-blob large_blob -- --nocapture
	@echo "==> blob smoke: PASS"

# Run S3 smoke tests against a running localstack instance.
# Requires: docker compose -f compose/test.yml up localstack
# and a pre-created bucket: aws --endpoint-url=http://localhost:4566 s3 mb s3://rb-blobs
blob-smoke-s3:
	@echo "==> blob smoke: s3 roundtrip (localstack)"
	RB_BLOB_S3_ENDPOINT=http://localhost:4566 \
	RB_BLOB_S3_BUCKET=rb-blobs \
	AWS_ACCESS_KEY_ID=test \
	AWS_SECRET_ACCESS_KEY=test \
	AWS_REGION=us-east-1 \
	cargo test -p rb-blob s3_roundtrip --features s3 -- --nocapture
	@echo "==> blob smoke s3: PASS"
