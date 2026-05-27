# ADR-012: Wave 8 — Hardening & Polish (admin, observability, trace surfacing, CI required-checks, E2E coverage, synthetic load)

**Status:** Proposed
**Date:** 2026-05-27
**Wave:** 8 (Phase 7 — Hardening & Polish)
**Author:** Architect
**Supersedes:** —
**Related:** Phase-7 epic; Wave 8 wave-level plan; ADR-007 (semantic graph), ADR-008 (Wave 6 ingestion), ADR-009 (agent execution), ADR-010 (GitHub App tenant install), ADR-011 (dev-stack auto-rebuild)

## Source plan and approvals

| Artifact | Reference | Notes |
|---|---|---|
| Wave 8 plan | Wave 8 wave-level plan (rev 1) | Pre-prod 7-day operator-free synthetic-load exit |
| Plan approval | Board `request_confirmation` accepted | Accepted 2026-05-27 |
| S7 carve-out | Plan §4.1 option (a) | Board accepted CTO recommendation to add synthetic-load harness as a 7th stream |

This ADR is the Gate-1 deliverable for Wave 8. The seven implementation streams (S1–S7) are blocked on this ADR and auto-unblock when it reaches `done`.

---

## 1. Context

Wave 8 is the final pre-prod wave of Phase 7. It hardens what Waves 1–7 shipped — it does **not** add new product surface. The Wave 8 exit criterion is structural rather than functional:

> Pre-prod deployment runs 7 days under synthetic load with zero operator intervention.

"Zero operator intervention" forces three properties to be true *simultaneously* on the running pre-prod stack:

1. **The stack is operable without an engineer in the loop.** Admin bootstrap, tenant-recovery, and impersonation flows that today require manual SQL or manager-only Paperclip moves must have first-class HTTP endpoints with audit logging. → **S1**.
2. **The stack is observable without an engineer in the loop.** Metrics, dashboards, and trace correlation must be in place so a human only looks when a paging signal fires — not as a routine pre-prod sanity check. → **S2, S3, S4**.
3. **The stack is provably correct without an engineer in the loop.** CI required-checks must be tight enough that a green PR is genuinely production-safe, and E2E coverage must be wide enough that catastrophic regressions surface before the synthetic-load run starts. → **S5, S6**.

The 7-day run itself needs a generator — operator-free can't be operator-driven — so a synthetic-load harness is carved as **S7** rather than deferred to a post-Wave-8 gate.

Wave 8 inherits a substantial amount of pre-existing infrastructure from earlier waves; this ADR is therefore mostly contracts and integration constraints, not greenfield design. Notable inheritance:

- `crates/rb-tracing` (Wave 5/6) already emits `trace_id` and `span_id` in structured logs and propagates them through Kafka headers. S4 is the *surfacing* of that surface to HTTP responses and the frontend — not new instrumentation.
- `services/control-api/src/routes/admin/*` already exists (`admin/github/app_manifest`, `admin/partition_maintenance`). S1 is the addition of cross-tenant operator endpoints under the same module, not a new admin surface.
- `infra/prometheus/`, `infra/grafana/`, `infra/otel-collector/` already provision under `compose/dev.yml`; Prometheus already scrapes (per the Wave 7 telemetry restoration fix). S2/S3 are the *naming convention* and *dashboard catalogue*, not the deployment of the agents.
- `frontend/playwright.config.ts` plus ~14 existing specs (`smoke`, `wave6-code-intel`, `wave7-happy-path`, `axe-*`, `session-replay`, etc.) are the baseline Playwright suite. S6 is the coverage-map work that promotes a subset of those to *required* CI gates with multi-browser cadence.
- `frontend/src/pages/TraceViewerPage.tsx`, `frontend/src/api/hooks/useTraceViewer.ts`, and `frontend/src/api/tempo.ts` were scaffolded during Wave 7 to unblock UX prototyping. S4's frontend contract must align with those file paths so the prototype work converges rather than being thrown away.

Wave 8 is the wave where these scaffolds become contracts. The risk we are managing is **drift between the scaffold and the contract** — a frontend component that calls `/api/admin/foo` when the server ships `/api/admin/v1/foo`, a Grafana panel that queries `rb_kafka_lag` when the metric is published as `rb_kafka_consumer_lag`. The single-document-covers-all-streams approach is deliberate: the cross-stream wire format constraints (S2 → S3, S4 backend → S4 frontend, S6 → S4, S1 → S5) are easier to keep aligned in one ADR than in six.

---

## 2. Decision

Wave 8 ships as **seven independent implementation streams** governed by **one wave-level ADR (this document)**, with per-PR Gate-2/Gate-3 review on the normal cadence. There are **no per-stream sub-ADRs**; if a stream's complexity outgrows its section here during implementation, the implementing engineer files a separate ADR and the PR blocks on its acceptance — but the assumption is that does not happen in Wave 8 because every stream builds on already-shipped infrastructure.

Each stream below specifies five things: the **goal at exit**, the **surface contract** (the user-visible / operator-visible / wire-format part that other streams depend on), the **cross-stream dependencies**, the **risks / open questions**, and where applicable a **threat model / invariants** sub-section. The streams are numbered to match the wave-level plan.

### 2.1 S1 — Admin bootstrap & impersonation (REQ-AD-01)

#### 2.1.1 Goal at exit

An operator with the `RB_ADMIN_TOKEN` secret can, **via HTTP only and without database access**, complete every break-glass action that today requires SQL or manager-only Paperclip moves:

- Bootstrap the first admin user when a fresh stack starts with zero accounts.
- Re-bind a tenant whose GitHub App install row drifted (e.g. cross-tenant hijack incidents we have seen before).
- Impersonate a tenant user for support purposes, with every action audit-logged and time-bounded.
- Force-delete a tenant and its data when a customer leaves or a test fixture must be cleaned.

The goal is **operability**, not feature delivery — every endpoint here corresponds to a manual recovery path we have already run during an incident.

#### 2.1.2 Surface contract

All endpoints live under `/api/admin/v1/` and require the `Authorization: Bearer $RB_ADMIN_TOKEN` header. They are mounted in `services/control-api/src/routes/admin/` alongside the existing `admin/github/*` and `admin/partition_maintenance` modules.

| Method | Path | Purpose | Idempotent? |
|---|---|---|---|
| `POST` | `/api/admin/v1/bootstrap/admin` | Create the first admin user when `auth.users` has zero rows. Refuses (`409 Conflict`) if any user exists. | Conditionally — `409` once first user exists, so re-runs are safe. |
| `POST` | `/api/admin/v1/tenants/:tenantId/rebind-gh-install` | Re-bind a GitHub App installation to a tenant; rejects if install row already points at a *different* tenant unless `force: true` is in the body **and** a `reason` is supplied. | No — explicit `force` semantics for the cross-tenant case. |
| `POST` | `/api/admin/v1/tenants/:tenantId/impersonate` | Mint a time-bounded (≤15 min, server-enforced ceiling) session token for the named tenant; **every subsequent request made with that token carries an `X-Impersonator-Admin-Id` header set by the gateway and writes an audit-log row per request**. | No — each call mints a new session. |
| `POST` | `/api/admin/v1/tenants/:tenantId/force-delete` | Two-phase: returns a `confirm_token` plus a snapshot of what will be deleted (counts per table); a second call with that token within 60 s actually deletes. | No — two-phase by design. |
| `GET` | `/api/admin/v1/audit-log?tenant_id=…&from=…&until=…` | Query audit log; tenant-scoped or global. | Yes. |

**RB_ADMIN_TOKEN model.** A single shared secret loaded from environment, present in `compose/dev.yml` for the dev stack and rotated via the existing `compose/scripts/rotate-secrets.sh` (Wave 7) for pre-prod. Token rotation invalidates outstanding impersonation sessions because the JWTs are signed with a key derived from the admin token + a per-mint nonce. The token is **never** logged, never appears in error messages, and is verified by constant-time compare. There is exactly **one** valid admin token at a time — no multi-token support, no per-operator subkeys; the audit log is what attributes individual operator actions, via an `X-Admin-Actor` request header that the operator supplies.

**Audit-log shape.** A new table `auth.admin_audit_log` (migration filed under `migrations/control-api/`):

```sql
CREATE TABLE auth.admin_audit_log (
    id              BIGSERIAL PRIMARY KEY,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    actor           TEXT NOT NULL,                     -- value of X-Admin-Actor; required (rejected if missing)
    action          TEXT NOT NULL,                     -- e.g. 'bootstrap.admin', 'tenant.rebind_gh', 'tenant.impersonate.start'
    tenant_id       UUID,                              -- nullable for global actions
    target_user_id  UUID,                              -- nullable
    request_id      UUID NOT NULL,                     -- the request's X-Request-Id / trace_id
    ip              INET,
    user_agent      TEXT,
    payload_summary JSONB NOT NULL,                    -- redacted body summary (never raw secrets)
    outcome         TEXT NOT NULL CHECK (outcome IN ('ok','denied','error')),
    error_class     TEXT
);
CREATE INDEX admin_audit_log_tenant_idx  ON auth.admin_audit_log (tenant_id, created_at DESC);
CREATE INDEX admin_audit_log_actor_idx   ON auth.admin_audit_log (actor, created_at DESC);
CREATE INDEX admin_audit_log_request_idx ON auth.admin_audit_log (request_id);
```

**Invariants** (these are non-negotiable; PR review must verify each):

1. **Every admin endpoint writes exactly one audit row per request, on every code path** — success, denial, and 5xx. Endpoints that handle the request without writing a row are considered broken.
2. **`X-Admin-Actor` is required.** No actor → `400 Bad Request` *before* any business logic runs. A missing actor must still result in an audit row (with `outcome='denied'`, `error_class='missing_actor'`).
3. **`payload_summary` never contains the raw `RB_ADMIN_TOKEN`, raw session tokens, or raw secret material.** PR review checks this by grepping the serializer.
4. **Impersonation sessions are server-time-bounded.** The JWT carries `exp` ≤ `now + 15 min`; the verifier rejects later `exp` claims even if signed correctly. Defense in depth against accidental long-lived tokens.
5. **`force-delete` is two-phase.** A single-call delete is not acceptable. Phase-1 returns a `confirm_token` bound to the actor + tenant + snapshot hash; phase-2 verifies all three before running.

#### 2.1.3 Threat model (non-negotiable per plan §4.3)

| Threat | Mitigation |
|---|---|
| **RB_ADMIN_TOKEN leak via logs.** | Constant-time compare in `verify_admin_token`; structured logger has an explicit denylist for the header name and `Authorization`; PR review greps for `RB_ADMIN_TOKEN` in test fixtures. |
| **RB_ADMIN_TOKEN leak via error messages.** | The admin auth middleware returns generic `401 Unauthorized` with no body content; never echoes the supplied token. |
| **Replay of impersonation token after admin token rotation.** | Impersonation JWTs are signed with `HMAC(admin_token, "imp:" || nonce)`; rotating `admin_token` invalidates all outstanding signatures. |
| **Cross-tenant data access via impersonation.** | The minted session carries the impersonated user's `tenant_id`; the existing tenant-scoped RLS in `crates/rb-storage-pg` enforces tenant isolation downstream. Impersonation does not bypass RLS — it sets the session's tenant context. |
| **Force-delete on the wrong tenant** (prior cross-tenant install incident pattern). | Phase-1 response includes a row-count snapshot per affected table; phase-2 token binds to a hash of that snapshot; a tenant whose row-counts shifted between the two calls forces a re-run (server returns `409`). |
| **Audit-log tampering / deletion.** | The audit log is in the same database as the data it protects, which is the wrong threat boundary in principle — but the operational answer is "operator with `RB_ADMIN_TOKEN` *can* delete it; that's why we copy it off." A nightly export to S3 (or equivalent) is out of scope for Wave 8 and tracked as a Wave 9 follow-up. The Wave 8 expectation is that the table exists and is correct; off-site retention is a Wave 9 concern. |
| **Admin endpoints exposed publicly.** | Reverse proxy / Caddy config restricts `/api/admin/v1/*` to a `Tailscale`/VPN IP allowlist *in addition to* the bearer-token check. Both gates required. Configured in `compose/tailscale.yml` and documented in the runbook. |
| **Operator without `X-Admin-Actor` doing untraceable actions.** | Middleware rejects missing actor *before* business logic; the rejection itself is audit-logged so the attempt is visible. |

#### 2.1.4 Cross-stream dependencies

- **S2 (metrics):** admin endpoints expose `rb_admin_requests_total{action,outcome}` counter (no high-cardinality labels — no actor, no tenant_id label) per §2.2.2 naming convention. Tenant-scoped admin failures show up in the dashboard as a spike on `outcome="denied"` without leaking tenant identifiers into Prometheus.
- **S3 (Grafana):** the **"Operability"** dashboard includes an admin-actions panel and an audit-log freshness panel.
- **S4 (trace-id):** every admin request gets a trace-id and the audit row's `request_id` is that trace-id, making "what did the operator do during the 14:00 incident?" answerable by joining audit-log to traces in Tempo.
- **S5 (CI):** integration tests for admin endpoints land in `services/control-api/tests/integration_admin_*`, and the corresponding CI job is one of the 12 required checks.
- **S6 (Playwright):** none — admin endpoints have no UI in Wave 8. Operators use `curl` from the runbook.

#### 2.1.5 Risks / open questions

- **Audit-log off-site retention** is deferred to Wave 9. Pre-prod runs against a single DB; this is acceptable for Wave 8's "zero operator intervention" gate because the gate is about *not needing* operator intervention, not about resilience to an actor with the admin token.
- **Impersonation session UI surfacing.** In Wave 8 there is no banner in the tenant frontend showing "you are being impersonated"; impersonation is purely server-side. Frontend banner is a Wave 9 task once we have customers who would care.
- **RBAC for multiple admin operators.** Wave 8 ships with one admin token; the `X-Admin-Actor` header attributes individual actions but does not authenticate them. Multi-operator RBAC (per-operator key pairs, role-based limits) is explicitly out of scope; it is a Wave 9 follow-up.

### 2.2 S2 — Prometheus metrics (REQ-OB-02)

#### 2.2.1 Goal at exit

Every service that exists in `services/*` emits a documented set of Prometheus metrics, scraped by the existing Prometheus instance under `compose/dev.yml`. The metric **names follow a single naming convention**, **labels respect a cardinality budget**, and a `make metrics-doc` target generates `docs/metrics.md` from in-source registry comments so the documentation does not drift.

#### 2.2.2 Surface contract — naming convention

```
rb_<service>_<subject>_<unit>{...}
```

| Part | Rule |
|---|---|
| `rb_` | All metrics carry the project prefix. No exceptions. |
| `<service>` | Singular service name with underscores: `control_api`, `agent_runner`, `parse_worker`, `projector_pg`, etc. Matches the binary name with `-` → `_`. |
| `<subject>` | The thing being measured: `requests`, `kafka_lag`, `db_pool`, `outbox_age`, etc. Snake-case nouns. |
| `<unit>` | Prometheus convention: `_total` for monotonic counters, `_seconds` for durations (histograms in seconds, never milliseconds), `_bytes` for sizes, no suffix for gauges, `_ratio` for 0–1 fractions. |

Examples (canonical — implementers may add metrics that follow the same pattern):

| Metric | Type | Labels (low-cardinality) | Service |
|---|---|---|---|
| `rb_control_api_requests_total` | Counter | `route`, `method`, `status_class` (`2xx`/`4xx`/`5xx`) | control-api |
| `rb_control_api_request_duration_seconds` | Histogram | `route`, `method` | control-api |
| `rb_agent_runner_sessions_total` | Counter | `outcome` (`completed`/`failed`/`cancelled`) | agent-runner |
| `rb_agent_runner_active_sessions` | Gauge | — | agent-runner |
| `rb_kafka_consumer_lag` | Gauge | `service`, `topic`, `partition` (partition counts capped — see budget) | all consumers |
| `rb_db_pool_connections` | Gauge | `service`, `pool` (`primary`/`replica`/`audit`), `state` (`idle`/`busy`) | all DB-using services |
| `rb_outbox_age_seconds` | Gauge | `service`, `topic` | services emitting Kafka events |
| `rb_admin_requests_total` | Counter | `action`, `outcome` | control-api |
| `rb_session_failures_total` | Counter | `reason` (bounded enum) | agent-runner |
| `rb_build_info` | Gauge (always `1`) | `service`, `git_sha`, `version` | all services (already shipped via [`rb-build-info`](../../crates/rb-build-info/) per ADR-011 §2.4) |

#### 2.2.3 Label cardinality budget

Cardinality is the load-bearing constraint — a single unbounded label can grow the Prometheus TSDB faster than retention can prune it.

| Rule | Enforcement |
|---|---|
| **No `tenant_id` label on any metric.** | Tenants are unbounded; per-tenant breakdowns belong in logs/traces, not Prometheus. PR review checks. |
| **No `user_id`, `session_id`, `job_id`, `repo_id`, `commit_sha`, `branch_name`, or any other UUID/SHA label.** | Same reason. PR review checks. |
| **`route` label uses the matched route pattern, not the raw path.** | e.g. `/agents/:id/events`, not `/agents/abc-123/events`. Axum's `MatchedPath` extractor provides this. |
| **`status_class` label is the bucketed class, not the raw status.** | `2xx` not `204`. Cuts cardinality 10×. |
| **`partition` label on Kafka metrics is allowed up to 16 partitions per topic.** | Above 16, partition is omitted and only per-topic aggregates are exposed. (Today no topic exceeds 16.) |
| **Hard cap: every service must keep total active series ≤ 10,000.** | A pre-commit lint (`make metrics-cardinality-check`) runs against the in-source registry and rejects PRs that exceed the budget. |

#### 2.2.4 Cross-stream dependencies

- **S3 (Grafana):** every metric named above must have at least one panel on at least one dashboard, or be explicitly marked as `:operator-only` in `docs/metrics.md`. The Grafana dashboard catalogue queries these metric names directly.
- **S4 (trace-id):** metrics and traces share the `service` label name (not `service_name`, not `svc`). Tempo→Prometheus correlations use this.
- **S5 (CI):** `make metrics-cardinality-check` is one of the 12 required checks.
- **S1 (admin):** admin endpoints emit `rb_admin_requests_total` per the naming convention; no special-casing.

#### 2.2.5 Risks / open questions

- **Histogram bucket choice.** We adopt the Prometheus default buckets for `*_seconds` histograms initially (`{0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10}`). Per-route bucket tuning is a Wave 9 task once we have a week of pre-prod data to inform it.
- **OpenTelemetry vs raw Prometheus.** We continue using the `prometheus` Rust crate via `rb-metrics` (existing). OTEL Metrics SDK is a possible future migration but not in Wave 8; see ADR alternatives in §4.2.
- **Metric versioning.** Renaming a metric breaks dashboards and alerts. PR review treats metric renames as a breaking change requiring a deprecation period (metric exposed under both names for at least one wave).

### 2.3 S3 — Grafana dashboards (REQ-OB-03)

#### 2.3.1 Goal at exit

A Wave 8 operator can answer four questions from Grafana alone, **without writing a PromQL query**:

1. *Is the stack healthy right now?* — green/yellow/red at-a-glance.
2. *Where is the bottleneck if something is slow?* — per-service request latency, queue/lag.
3. *Did anything change recently?* — build SHAs, restart timeline, deploy log overlay.
4. *Who did what?* — admin actions and impersonation timeline.

These map to four provisioned dashboards (see catalogue).

#### 2.3.2 Surface contract — dashboard catalogue

Dashboards are **file-based** (provisioned from `infra/grafana/dashboards/*.json`) — not API-pushed — for three reasons: they live in source control, they are reviewable in PRs, and they survive a Grafana wipe. The `infra/grafana/provisioning/` directory already configures dashboard provisioning under `compose/dev.yml`.

| Dashboard file | Title | Audience | Key panels |
|---|---|---|---|
| `infra/grafana/dashboards/overview.json` | **Stack overview** | First-look operator | per-service up/down, error-rate sparkline, active sessions, kafka-lag heatmap, `rb_build_info` SHA table |
| `infra/grafana/dashboards/latency.json` | **Latency & throughput** | Performance triage | p50/p95/p99 per route, request rate, kafka topic throughput, DB pool utilisation, top-10 slowest routes |
| `infra/grafana/dashboards/changes.json` | **Changes & deploys** | "What changed?" triage | `rb_build_info` SHA timeline, deploy-event overlay, restart counts per service, panel linking each SHA to its merge PR |
| `infra/grafana/dashboards/operability.json` | **Operability & admin** | Incident response | `rb_admin_requests_total` by action/outcome, audit-log freshness gauge, session failure rates, queue depth |

**Provisioning path.** File-based only. Operators do not "save dashboard" from the UI; if they want a new panel, it lands in the JSON via a PR. The `infra/grafana/provisioning/dashboards.yaml` config has `allowUiUpdates: false` to make the file-vs-UI direction unambiguous.

**Datasource.** Single Prometheus datasource pre-provisioned via `infra/grafana/provisioning/datasources.yaml`. Tempo datasource added in S4 (trace-id) for trace correlation; both share a single `Grafana` instance.

**Panel-to-metric mapping is enforced.** A `make grafana-lint` target greps each `.json` panel's PromQL for metric names and verifies every referenced metric is in the S2 metric registry. Catches the "Grafana panel queries a metric that was renamed three commits ago" failure mode.

#### 2.3.3 Cross-stream dependencies

- **S2:** consumes every documented metric — Grafana lint is the closing of the S2/S3 contract.
- **S4 (trace-id):** the **Latency** and **Operability** dashboards include a `traceID` link column on any per-request panel (clicking a row pivots into Tempo). Requires Tempo datasource provisioned alongside Prometheus.
- **S5 (CI):** `make grafana-lint` is one of the 12 required checks.
- **S1 (admin):** **Operability** dashboard panels visualise `rb_admin_requests_total`.

#### 2.3.4 Risks / open questions

- **Alerting.** Grafana has alerting capability; Wave 8 ships **dashboards only**, no alerts. Alerting policy (where pages go, who is on call, severity matrix) is a Wave 9 task — pre-prod's "zero operator intervention" gate is about not *needing* an operator, not about routing pages to one.
- **Single-tenant view.** Dashboards aggregate across tenants because metrics aggregate across tenants (no `tenant_id` label). A per-tenant operations view requires a log/trace pivot, not a Grafana dashboard.

### 2.4 S4 — Trace ID surfacing (REQ-TR-03)

#### 2.4.1 Goal at exit

When something goes wrong in the frontend, the user (or the developer reading a bug report) can:

1. See a **trace ID** in the UI (in the error toast, in a footer, in a copy-to-clipboard control on the new trace-viewer page).
2. Paste that trace ID into the trace-viewer page (already scaffolded at `frontend/src/pages/TraceViewerPage.tsx`) to see the full distributed trace for that request, across all services that participated.
3. Click through to Tempo / Grafana for the same trace, with deep-link parameters pre-filled.

The backend half is already present (`crates/rb-tracing` emits `trace_id` to logs and Kafka headers); Wave 8 surfaces it to HTTP responses and the UI.

#### 2.4.2 Surface contract — backend (Services Engineer)

**HTTP propagation contract.** Every response from any service in `services/*` carries a single response header:

```
X-Trace-Id: <32-hex-chars>
```

If the incoming request carries `X-Trace-Id` (or the W3C `traceparent` header), it is reused; otherwise the server's tracing layer generates a new one in `crates/rb-tracing` and surfaces it. The middleware is centralised — every service registers it via a `rb-tracing::http::layer()` axum layer applied in `server.rs` — so we do not duplicate trace-id surfacing per route.

**Why `X-Trace-Id` and not just `traceparent`?**

- `traceparent` is the W3C standard and is also set — we do not drop it. But it encodes `<version>-<trace_id>-<span_id>-<flags>` and is awkward for humans to read and copy.
- `X-Trace-Id` is the **operator-facing** form: 32 hex chars, copy-pasteable into Tempo's search bar verbatim, no parsing required.

**Tempo lookup contract.** A new control-api endpoint:

```
GET /api/traces/:trace_id
→ 200 with a redirect to the Grafana Tempo deep-link URL (the frontend follows the redirect in a new tab), or
→ 404 if the trace ID is malformed (not 32 hex chars).
```

The redirect URL is built from a config value `RB_TEMPO_BASE_URL` (already referenced in `frontend/src/api/tempo.ts`). The trace existence check is *not* done server-side — the frontend opens the Tempo link, and Tempo handles "trace not found" with its own UI.

#### 2.4.3 Surface contract — frontend (Frontend Engineer)

**Three integration points** (the trace-viewer scaffolds already exist; this is the contract they implement, not new files):

1. **Trace ID in error responses.** The shared `client.ts` API wrapper reads `X-Trace-Id` from every response (success and failure) and stores it on the response/error object. Error toasts display the last 8 hex chars + a copy-full button.
2. **`TraceViewerPage.tsx`** (existing scaffold, `frontend/src/pages/TraceViewerPage.tsx`). User pastes a full 32-hex trace ID, hits Enter, and the page calls `GET /api/traces/:trace_id` and follows the redirect. The page also accepts `?trace_id=` query param for deep-linking from a bug report.
3. **`useTraceViewer.ts` hook** (existing scaffold) wraps the API call and the redirect; `tempo.ts` (existing scaffold) holds the `RB_TEMPO_BASE_URL` reading from `ImportMetaEnv` (`VITE_TEMPO_URL` per [PR #600](https://github.com/f-crop/rustacean/pull/600)).

**Storage shape.** The current trace ID is **not** persisted to localStorage / sessionStorage / cookies — it is per-request and lives on the response object. The `TraceViewerPage` is a read-only viewer; there is no "trace history" feature in Wave 8.

**Route registration.** `TraceViewerPage` mounts at `/trace-viewer` in the existing react-router config. Authenticated users only — public bug-report pasteboards are not in scope.

#### 2.4.4 Cross-stream dependencies

- **S2 (metrics):** `service` label on metrics matches the `service.name` resource attribute in OTLP-emitted spans so Tempo/Prometheus correlations work.
- **S3 (Grafana):** the **Latency** and **Operability** dashboards include `traceID` columns that link into Tempo.
- **S5 (CI):** an integration test verifies `X-Trace-Id` round-trips for at least three services (control-api, agent-runner, parse-worker); this is one of the 12 required checks.
- **S6 (Playwright):** at least one Playwright spec exercises the trace-viewer page (paste → redirect happens or error toast surfaces a trace ID).

#### 2.4.5 Risks / open questions

- **Tempo retention.** Tempo's storage tier (currently disk-backed under `infra/tempo/`) keeps traces for a finite window. Pre-prod operator workflow assumes a trace ID is useful for a short period after the request; a UI that surfaces "this trace ID may have expired from Tempo" is a Wave 9 polish.
- **Sampling.** Wave 8 ships with the existing 100% sample rate that pre-prod has been running on. A sampling decision (head-based vs tail-based) is a Wave 9 task when we have storage-cost data.
- **Frontend scaffold drift.** The pre-existing `TraceViewerPage` / `useTraceViewer` / `tempo.ts` files were written ahead of the backend; the implementer must verify they call the contract specified in §2.4.2 and §2.4.3 and patch them if not. We are choosing **server-shapes-frontend** here, not the reverse.

### 2.5 S5 — CI required-checks (REQ-DV-06)

#### 2.5.1 Goal at exit

A PR cannot merge to `main` unless **all 12 required checks** are green. The list is bound to the `main` branch protection rule and the binding procedure is documented so a new check can be added or rotated without ambiguity.

#### 2.5.2 Surface contract — the 12-job matrix

Existing CI workflows on `main` (per `.github/workflows/`): `ci.yml`, `dev-stack-drift-check.yml`, `frontend-axe-dispatch.yml`, `mcp-server-publish.yml`, `pipeline-e2e.yml`, `pr-bundle-check.yml`, `pr-hygiene.yml`, `pr-migration-hygiene.yml`, `runtime-smoke.yml`, `board-smoke.yml`. The required-checks list collapses these into 12 named status checks bound to branch protection:

| # | Required check (status name) | Workflow file | What it gates |
|---|---|---|---|
| 1 | `ci / build` | `ci.yml` | `cargo build --workspace --all-targets` |
| 2 | `ci / test` | `ci.yml` | `cargo test --workspace --all-features` |
| 3 | `ci / clippy` | `ci.yml` | `cargo clippy --workspace -- -D warnings` |
| 4 | `ci / fmt` | `ci.yml` | `cargo fmt --check` |
| 5 | `ci / deny` | `ci.yml` | `cargo deny check` |
| 6 | `ci / frontend-build` | `ci.yml` | `pnpm --filter frontend build` |
| 7 | `ci / frontend-test` | `ci.yml` | `pnpm --filter frontend test` + vitest unit |
| 8 | `pr-hygiene` | `pr-hygiene.yml` | branch name, title bracket-prefix (tracker-issue id or `[REQ-XX-NN]`), body links |
| 9 | `pr-bundle-check` | `pr-bundle-check.yml` | bundle rules, parent-decision xref hazard |
| 10 | `pr-migration-hygiene` | `pr-migration-hygiene.yml` | additive-only, checksum-stable migrations |
| 11 | `pipeline-e2e` | `pipeline-e2e.yml` | end-to-end ingestion pipeline smoke |
| 12 | `runtime-smoke` | `runtime-smoke.yml` | docker-stack boot + health probe |

Three more workflows (`dev-stack-drift-check`, `frontend-axe-dispatch`, `board-smoke`) exist on `main` but are **not** required-checks — they are advisory or scheduled. The required-checks list explicitly excludes them so a flaky scheduled job does not block merges.

**Metrics cardinality and Grafana lint are folded into `ci / test`** rather than introducing new required checks, to keep the count at 12.

#### 2.5.3 Branch-protection bind procedure

Branch protection is **declarative** via a checked-in `.github/branch-protection.yml`-equivalent (or a documented `gh api` script under `scripts/` if GitHub does not support declarative yet — Wave 8 picks whichever is reliable). The bind procedure:

1. Update the bind script / declarative file with the new required-checks list.
2. PR review pings PR Reviewer (who is allowed to merge with the new check passing).
3. Once merged, PR Reviewer runs `scripts/apply-branch-protection.sh` (idempotent) which calls `gh api repos/:owner/:repo/branches/main/protection -X PUT` with the new list.
4. A read-back verification (`gh api .../branches/main/protection`) runs in `runtime-smoke` to detect drift between the file and the live protection rules.

#### 2.5.4 Cross-stream dependencies

- **S1, S2, S3, S4, S6:** every other stream lands its tests inside the existing required checks (`ci / test`, `pipeline-e2e`, `runtime-smoke`) rather than introducing per-stream required checks. This is intentional — adding checks fragments the gate.
- **S7 (synthetic-load):** the harness itself is **not** a required-check (it runs out-of-band on pre-prod); it produces a report that is consulted at the wave-exit gate.

#### 2.5.5 Risks / open questions

- **Test flake rate.** A 12-check required list amplifies flakes 12×. The implementer must establish a baseline flake rate (re-run percentage over 100 PRs) before tightening. Mitigations: pin to specific runner images, isolate stateful integration tests with `--test-threads=1` where needed.
- **CI runtime.** End-to-end CI time grows with the matrix. Wave 8 target is ≤25 min critical-path; if `pipeline-e2e` or `runtime-smoke` exceeds that, parallelise inside the workflow rather than dropping a check.
- **Self-PR blocks.** GitHub blocks self-reviews; the CTO has merge-with-admin override for the canonical case (per `feedback_gh_self_review_blocked`). The branch-protection bind procedure preserves that override.

### 2.6 S6 — Playwright E2E coverage map (REQ-FE-11)

#### 2.6.1 Goal at exit

A **named, documented matrix** of E2E flows × browsers × CI cadences covers every user-visible feature that landed in Waves 1–7, with each flow either green-required-on-PR or scheduled-nightly-tracked. The synthetic-load harness (S7) reuses the same Page Object Model.

#### 2.6.2 Surface contract — coverage matrix

| Flow | Spec file | Browsers (PR) | Browsers (nightly) | Required-on-PR? |
|---|---|---|---|---|
| Smoke (login, home, health) | `frontend/tests/smoke.spec.ts` (exists) | Chromium | Chromium + Firefox + WebKit | yes (via `ci / test`) |
| Ingestion happy-path | `ingestion-live.spec.ts` (exists) | Chromium | Chromium + Firefox | yes (via `pipeline-e2e`) |
| Ingestion stage progression | `ingestion-stage-progression.spec.ts` (exists) | Chromium | Chromium + Firefox | yes |
| Code-intel queries (Wave 6) | `wave6-code-intel.spec.ts` (exists) | Chromium | Chromium + Firefox | yes |
| Wave-7 happy-path | `wave7-happy-path.spec.ts` (exists) | Chromium | Chromium + Firefox | yes |
| Agent execution flow | `agent-execution.spec.ts` (exists) | Chromium | Chromium + Firefox | yes |
| Session replay | `session-replay.spec.ts` (exists) | Chromium | Chromium + Firefox | yes |
| Repos connect (GitHub App install) | `repos-connect.spec.ts` (exists) | Chromium | Chromium + Firefox | yes |
| Query refetch / cache | `query-refetch.spec.ts` (exists) | Chromium | Chromium | yes |
| Accessibility (axe) scan | `axe-scan.spec.ts` (exists) | Chromium | Chromium | yes |
| Accessibility dispatch | `axe-dispatch.spec.ts` (exists) | Chromium | Chromium | scheduled (`frontend-axe-dispatch.yml`) |
| Keyboard escape dismissal | `escape-dismissal.spec.ts` (exists) | Chromium | Chromium | yes |
| Focus trap | `focus-trap.spec.ts` (exists) | Chromium | Chromium | yes |
| Form-field validation | `field-validation.spec.ts` (exists) | Chromium | Chromium | yes |
| **Trace viewer (new, S4)** | `trace-viewer.spec.ts` (new) | Chromium | Chromium + Firefox | yes |
| **Admin operator UI (none)** | — | n/a — admin is CLI-only in Wave 8 | n/a | n/a |

Roughly half the matrix exists already from Waves 5–7; Wave 8 adds **one new spec** (`trace-viewer.spec.ts`) and promotes the matrix from "tests that pass" to "tests that gate PRs". The QA Engineer's main work is the **cadence + reliability tier** assignment per spec, not net-new spec authoring.

**Page Object Model.** `frontend/tests/pages/` (existing) — extended for the trace-viewer page. POMs must be reused by S7 (synthetic load) so that the test pyramid and the load pyramid share intent.

**Browser cadence.** PR runs use Chromium only by default (per matrix). Nightly cron runs the WebKit / Firefox extensions for the flagged specs. Three reasons: (a) Chromium is the most-used browser for the customer base we are targeting pre-prod; (b) WebKit/Firefox runners are slower; (c) the nightly cron catches the cross-browser regressions without inflating PR time.

#### 2.6.3 Cross-stream dependencies

- **S4 (trace-id):** the new `trace-viewer.spec.ts` depends on S4's frontend contract; if S4 lands first, the spec lands with it; if S6 lands first, the spec is gated until S4 ships.
- **S5 (CI):** Playwright matrix invocations live inside `ci / test` (the required check). Nightly cadence lives in a separate cron-scheduled workflow.
- **S7 (synthetic-load):** S7 reuses Playwright's POM and Tempo trace assertion patterns.

#### 2.6.4 Risks / open questions

- **Flake budget.** Playwright tests are notoriously flaky against real backends. The QA Engineer establishes a per-spec flake budget (re-run on first failure); chronically flaky specs are quarantined to nightly and tracked as bugs, not silently disabled.
- **Tenant fixtures.** Each spec needs a fresh tenant or a tear-down-and-recreate. Pre-prod's "zero operator intervention" exit run uses S1's `force-delete` for tenant cleanup between iterations.

### 2.7 S7 — Synthetic-load harness (wave-exit harness)

#### 2.7.1 Goal at exit

A single command (`scripts/synthetic-load.sh start` or a `make pre-prod-soak`) runs an unattended workload against pre-prod for **at least 7 days**, **without** an operator restarting it, **and** produces a daily summary in `~/.local/state/rustbrain/synthetic-load/<date>.json` containing health, throughput, error rate, and a pass/fail verdict against the Wave 8 exit thresholds.

S7 is **the** Wave 8 exit gate — every other stream is means-to-end for S7.

#### 2.7.2 Surface contract

**Workload composition.** The harness drives a mix of three loops, each parameterised by rate and concurrency:

| Loop | Drives | Why it matters |
|---|---|---|
| **Ingestion loop** | `POST /repos/connect` → wait for `done` event → query results | Exercises Kafka, all 11 Rust workers, Postgres + Neo4j + Qdrant. Highest-impact synthetic. |
| **Agent execution loop** | `POST /agents/:id/sessions` → poll → terminate | Exercises agent-runner, control-api SSE, session lifecycle. |
| **Query loop** | `POST /search` (semantic + structural) | Exercises read path, embedding cache, semantic graph. |

Each loop has a target rate (RPS or sessions/min) sized to pre-prod's capacity at ~30% utilisation — the soak is about *sustained correctness*, not load-testing the upper bound.

**Tenant management.** The harness creates and force-deletes tenants on a rotation (~10 active tenants at any time). S1's force-delete endpoint is the cleanup mechanism. Tenant identifiers are deterministic (`synth-load-<n>`) so the operator can join logs across runs.

**Health gating.** Every iteration:

1. Hits `GET /health` on each service. Any non-200 records a failure.
2. Hits `GET /health/build` on each service. Any unexpected SHA records a drift event.
3. Reads `rb_outbox_age_seconds` and `rb_kafka_consumer_lag` — values above thresholds (see runbook) record a degradation event.
4. Captures the `X-Trace-Id` of the last failed request and saves it to the daily summary so the operator can pivot to Tempo if they review the run.

**Pass/fail verdict.**

| Metric | Threshold |
|---|---|
| Service availability (up/total samples) | ≥ 99.5% |
| Ingestion-loop success rate | ≥ 99% |
| Agent-loop success rate | ≥ 95% (agent exits can fail for reasons unrelated to platform) |
| Query-loop p95 latency | ≤ 2 s |
| Kafka consumer lag (any topic, any sample) | < 10,000 messages, never sustained > 1 minute |
| Outbox age (any topic, p95) | < 60 s |

**Operator-free invariant.** The harness must **not** require human intervention to recover. If the stack briefly degrades (a service restarts, a Kafka consumer rebalances) the harness keeps running and records the degradation but does not exit. The only events that legitimately exit the harness are a **catastrophic** failure (e.g. database unreachable for >5 min) — and even then it produces a final summary and exits with a non-zero code that the operator sees the next time they check.

**Implementation language.** Rust binary under `services/synthetic-load/` (new service), built into the existing image fleet. Drives the workload via `reqwest` and shares the existing OpenAPI client where possible. Reuses Playwright POMs **only** for the UI portion of the agent-loop assertions (small fraction); the bulk is HTTP. We **do not** drive the harness from Playwright headlessly across 7 days because Playwright's browser context is not designed for week-long sessions.

#### 2.7.3 Cross-stream dependencies

- **S1 (admin):** tenant cleanup uses `force-delete`. **S7 blocks on S1.**
- **S2 (metrics):** health gating reads documented metrics. S7 blocks on S2.
- **S3 (Grafana):** an operator who *chooses* to look during the soak uses S3 dashboards. Not a hard dependency.
- **S4 (trace-id):** failure capture writes the failing trace ID into the daily summary. S7 blocks on S4.
- **S5 (CI):** S7 is **not** a required-check. It is invoked manually on the pre-prod host.
- **S6 (Playwright):** POM reuse — soft dependency (the harness could ship without POM reuse if S6 lags).

#### 2.7.4 Risks / open questions

- **State accumulation.** A 7-day soak with continuous tenant churn produces a lot of data. Disk usage on pre-prod is the most likely failure mode. The harness reports disk usage in the daily summary and a Grafana panel on the **Operability** dashboard visualises trend.
- **Pre-prod capacity sizing.** The exit thresholds above are calibrated to pre-prod's expected capacity, not staging-test capacity. If pre-prod hardware specs change between ADR acceptance and Wave 8 exit, the thresholds need re-validation.
- **Resume semantics.** If the harness host (mars or pre-prod runner) reboots mid-soak, the harness restarts from the last persisted iteration counter — it does not re-start the 7-day clock from zero unless the gap exceeds 1 hour. The 1-hour gap heuristic is the line between "blip" and "loss of soak credibility".

---

## 3. Consequences

**One ADR governs the whole wave.** Every Wave-8 implementation PR cites this ADR section in its description and body. PR review checks that the surface contract in the PR matches the surface contract in this ADR; deviations require an ADR amendment before merge, not a follow-up PR. This is the lever that prevents the "scaffold-vs-contract drift" failure mode flagged in §1.

**Streams ship in parallel after this ADR is accepted.** Per the plan §3 sequencing, all seven streams kick off simultaneously once Gate-1 is `done`. Cross-stream coupling points are explicit in each section's *Cross-stream dependencies* paragraph; the most common couplings are S1→S5 (admin tests required in CI), S2→S3 (Grafana lint enforces metric name match), and S4→S6 (trace-viewer spec gates on the trace-id endpoint). Implementers communicate via the wave epic when a coupling point becomes a blocker.

**The synthetic-load harness is the wave's exit oracle.** Per §2.7.2, the harness produces a structured daily summary with a pass/fail verdict against thresholds. The Wave-8 done-gate (UAT Engineer's responsibility) reads those summaries; it does **not** re-run the soak from scratch. The thresholds in §2.7.2 are the contract — if pre-prod cannot sustain them, Wave 8 does not exit, and the failure mode is recorded in a Wave-8 retro under the epic.

**Existing scaffolds are honoured, not rewritten.** The trace-viewer frontend files (§2.4.3), the existing Playwright specs (§2.6.2), and the Prometheus/Grafana/OTel provisioning (§2.2, §2.3) are pre-existing. Implementers patch them to match the contract rather than starting fresh files. The single exception is S7 (`services/synthetic-load/`), which is genuinely net-new.

**No new shared crate is required.** All seven streams build on existing `crates/rb-*` (`rb-tracing`, `rb-metrics`, `rb-build-info`, `rb-storage-pg`, `rb-kafka`). S1's `auth.admin_audit_log` migration lives under `migrations/control-api/`. S7 may add a small `services/synthetic-load/` crate-binary but its dependencies are existing crates. This keeps Wave 8 inside the architectural boundary (no new cross-cutting library, no new boundary risk).

**The CI gate becomes the steady-state surface.** Once S5's required-checks list is bound, post-Wave-8 changes to the list require an ADR amendment. The list is intentionally short (12 checks); adding the 13th is a board decision because it's a permanent narrowing of the merge funnel.

**Wave 8 explicitly does not deliver:** off-site audit-log retention (S1.5 follow-up), Grafana alerting (S3 follow-up), histogram bucket tuning (S2 follow-up), trace sampling policy (S4 follow-up), and multi-operator admin RBAC (S1 follow-up). These are recorded under §5 and filed as Wave-9 backlog candidates.

---

## 4. Alternatives considered

### 4.1 Per-stream ADRs (one per of the seven streams)

Write seven separate ADRs, one per stream, and run a Gate-1 confirmation on each.

**Rejected.** The streams share more contract surface than they share independence: S2's metric names appear in S3's dashboards and S7's health checks; S4's `X-Trace-Id` appears in S1's audit log and S6's trace-viewer spec; S5's required-checks list cites the test surfaces of every other stream. Authoring those couplings across seven documents would produce either repetition (each ADR re-states the cross-stream constraint) or drift (each ADR diverges from the others' interpretation). One wave-level ADR keeps the contract surface visible in one place.

The cost of the one-document choice is reviewer load — this ADR is long. We accept that cost because the alternative is harder to keep aligned across seven concurrent PRs.

### 4.2 OpenTelemetry Metrics SDK (replace `prometheus` crate)

Migrate `rb-metrics` from the `prometheus` Rust crate to the OpenTelemetry Metrics SDK, exposing metrics via OTLP rather than the Prometheus scrape protocol.

**Deferred, not rejected.** OTEL metrics is a credible future direction (S4 already uses OTEL traces via OTLP), and unifying logs/metrics/traces under one SDK has obvious operational appeal. But (a) the existing `prometheus` crate works, (b) the existing OTel collector under `infra/otel-collector/` already bridges OTLP to Prometheus when needed, and (c) Wave 8 is a hardening wave, not a migration wave. Re-platforming the metrics stack inside Wave 8 would inject the largest fleet-wide change of the wave into the wave with the tightest exit criterion. We pin to Prometheus for Wave 8 and reserve the OTEL migration as a Wave-9 or Wave-10 ADR.

### 4.3 Defer S7 (the synthetic-load harness) to Wave 8.5 / Wave 9

Treat the 7-day soak as a post-Wave-8 gate; exit Wave 8 on the other six streams.

**Rejected by the board** (Wave 8 plan §4.1 option (b) considered and declined). The reason: the soak isn't a *test* of Wave 8, it's the *operational outcome* Wave 8 was scoped to deliver. If we exit Wave 8 without it, we exit Wave 8 without evidence that the wave's stated exit criterion holds. The cost of carving S7 as a 7th stream (~2 weeks of Platform engineer time) is small relative to the cost of declaring Wave 8 done on a criterion we haven't validated.

### 4.4 Operator-watched soak instead of `S7` harness

Run the 7-day soak with an operator who watches Grafana and restarts services when needed; ship Wave 8 with the operator-watch flow documented in the runbook.

**Rejected** because it violates the wave's exit criterion ("zero operator intervention"). A wave that exits on a criterion it does not actually meet is a wave that produces operational debt — the post-wave reality is that nobody knows if pre-prod is genuinely operator-free.

### 4.5 Push-based dashboards (Grafana API rather than file-provisioned)

Manage Grafana dashboards via the Grafana HTTP API rather than as JSON files under source control.

**Rejected.** Three reasons: (a) source-controlled JSON is reviewable in PRs in the same way every other config is; (b) `grafana-lint` (metric-name verification) is straightforward against a JSON file and hard against a live Grafana; (c) API-pushed dashboards are lost when Grafana storage is wiped, which is exactly the kind of operational fragility Wave 8 is supposed to eliminate.

### 4.6 Combine S1 (admin) with Wave-9 RBAC

Defer admin endpoints until we have multi-operator RBAC; ship Wave 8 with the existing SQL-only admin flows.

**Rejected.** The same reasoning as §4.3 / §4.4: the operator-free exit criterion requires HTTP admin endpoints, period. Multi-operator RBAC is a follow-on improvement that does not change the Wave-8 contract.

---

## 5. Open follow-ups (not part of this ADR's acceptance)

These are explicitly out of scope for Wave 8 and tracked for Wave 9 / Wave 10:

- **Audit-log off-site retention** (Wave 9 follow-up). Wave 8 stores audit-log in the same database as the data it protects; off-site export is a Wave 9 concern.
- **Frontend impersonation banner** (Wave 9 follow-up). "You are being impersonated by admin X" surface in the tenant UI. Wave 8 ships impersonation server-side only.
- **Multi-operator admin RBAC.** Wave 8 ships with one shared `RB_ADMIN_TOKEN`; per-operator key-pairs and role limits are a Wave 9 follow-up.
- **Grafana alerting policy.** Dashboards are sufficient for "zero operator intervention" pre-prod; alerting (where pages go, on-call rotation) is a GA-readiness concern.
- **Histogram bucket tuning.** Per-route bucket optimisation after a week of pre-prod data informs the tuning.
- **Trace sampling policy.** Wave 8 ships 100% sampling; head-based vs tail-based sampling decision after pre-prod storage-cost data is available.
- **OTEL metrics migration.** Whether to unify on OTEL SDK across logs/metrics/traces is a Wave-9 or Wave-10 ADR (§4.2).
- **Multi-host dev-stack / pre-prod redundancy.** Currently single-host; geographic redundancy is a post-GA topic.

---

## 6. Acceptance checklist (for board reviewers)

A reviewer accepting this ADR is confirming the following:

- [ ] The seven-stream decomposition correctly partitions Wave 8 scope (no missing surface, no overlap).
- [ ] S1's admin endpoints, audit-log shape, and threat model are sufficient for "operator-free pre-prod" — i.e. the listed endpoints cover every break-glass action operators currently take.
- [ ] S2's metric naming convention and cardinality budget are restrictive enough to keep TSDB growth tractable for the 7-day soak.
- [ ] S3's four-dashboard catalogue answers the four operator questions in §2.3.1 from Grafana alone.
- [ ] S4's `X-Trace-Id` propagation and trace-viewer route are sufficient for a user/developer to pivot from a UI error to a Tempo trace.
- [ ] S5's 12-job required-checks list strictly improves the existing gate set without introducing flake amplification beyond the wave's tolerance.
- [ ] S6's Playwright coverage matrix protects the user-visible features shipped in Waves 1–7 against regression during S7.
- [ ] S7's harness, thresholds, and resume semantics are sufficient evidence for Wave 8 exit.
- [ ] The cross-stream dependencies in each section are correctly identified (no hidden coupling).
- [ ] The follow-ups in §5 are correctly deferred (none of them are load-bearing for the Wave-8 exit criterion).

Approve via the `request_confirmation` interaction filed on the wave-level ADR document on the tracker.
