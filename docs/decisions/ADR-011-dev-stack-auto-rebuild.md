# ADR-011: Dev-stack auto-rebuild watcher + canonical compose-dir invariant

**Status:** Accepted
**Date:** 2026-05-25
**Wave:** 7 (consolidation of decisions shipped 2025-Q4 → 2026-05-25)
**Author:** Architect
**Supersedes:** —
**Related:** `docs/dev-stack-auto-rebuild.md` (operator detail), `dev-deployment` skill (mars-specific recovery)

## Source PRs

Every claim in this ADR is traceable to a merged PR on `main`.

| Stage | Paperclip issue | PR | Commit | What shipped |
|-------|-----------------|----|--------|--------------|
| 1 | RUSAA-1185 | #373 | `7077d107` | Wired the git `post-merge` hook (`scripts/install-git-hooks.sh` → `core.hooksPath=.githooks`); `agent-runner` added to the rebuild set. |
| 2 | — | #285 | `84966d09` | Selective rebuild expanded to **all 10** Rust services on shared-source (`crates/**`, `Cargo.{toml,lock}`, `proto/**`) changes. |
| 3 | RUSAA-1537 | #480 | `ba5c8d32` | `infra/systemd/rustbrain-dev-watch.service` installed as **user** systemd on mars (`loginctl enable-linger jarnura`), survives logout. |
| 4a | RUSAA-1642 | #566 | `74272740` | `rb-build-info` crate + `/health/build` SHA endpoint + CI SHA gate (Phase 1). |
| 4b | — | #568 | `6025e907` | `rb-build-info` fleet rollout to all 11 services (Phase 2). |
| 4c | RUSAA-1644 | #570 | `af9624c5` | Agent-runner / MCP SHA pairing — warn-only mismatch detection (Phase 3). |
| 5 | RUSAA-1653 | #572 | `eff8ed51` | Watcher pinned to **canonical** repo compose dir; refuses to run under `/tmp/`; `ConditionPathNotEmpty=…/compose/active-env` sentinel on the unit. |

---

## 1. Context

UAT for Rustacean runs against `main` HEAD on a single host (mars). There is no GHCR push from CI: every custom image (`ghcr.io/jarnura/rustacean/*:dev`, `rustbrain/frontend:dev`) is built **on mars from source**, and `compose/dev.yml` carries `pull_policy: never` on every custom service to make registry pulls impossible by construction. This is a deliberate operational invariant — mars holds no GHCR credentials, and we do not want a half-credentialled rebuild to silently fall back to an older image from the registry.

That choice forces the deployment surface onto mars itself. Whatever ships to UAT is whatever the local `docker compose build` produced from whatever `main` SHA the local working tree is at. Four properties had to hold for this to be tractable:

1. **Selective per changed path** — a docs-only commit must not rebuild 11 Rust services; a `crates/**` change must rebuild *all* dependent services because the shared crate is a transitive dependency.
2. **Safe to re-run** — rebuilds are idempotent (the same SHA range can be replayed without corrupting state); migrations skip already-applied versions.
3. **Survives logout** — the watcher cannot be a foreground tmux pane or a `nohup` job. On any reboot, kernel upgrade, or accidental session close, UAT would silently freeze on a stale `main`.
4. **Pinned to a canonical repo dir** — multiple clones of the repo on mars (legitimate: hotfix branches, scratch debugging) must not race for ownership of the dev-stack containers. A clone under `/tmp/phase2-deploy/` previously hijacked the `com.docker.compose.project.working_dir` label and broke bind-mounts when the directory was cleaned up.

Wave 7 ratified the five-stage evolution that delivered these four properties incrementally. Stages 1–3 built the watcher itself; Stage 4 added cryptographic evidence (build SHA) so that "is what's deployed actually what merged?" became a queryable question instead of a folk-belief; Stage 5 closed the canonical-dir loophole that had been silently corrupting UAT.

---

## 2. Decision

We run **one** auto-rebuild watcher per host, owned by user systemd, pinned to the canonical repo clone, with three orthogonal correctness guarantees: selective rebuild rules, build-SHA evidence, and a canonical-dir invariant enforced both in-script and in the unit file. Each stage shipped independently and is documented here as one consolidated story.

### 2.1 Stage 1 — Post-merge hook + agent-runner inclusion (RUSAA-1185, PR #373, `7077d107`)

Local dev boxes get the rebuild via a `post-merge` git hook checked in under `.githooks/`, enabled with `scripts/install-git-hooks.sh` (sets `core.hooksPath = .githooks`). The hook backgrounds `scripts/dev-stack-auto-rebuild.sh` with the `ORIG_HEAD → HEAD` range so the rebuild does not block the `git pull` returning. `agent-runner` joined the rebuild set in this PR — previously omitted because it shipped after the original watcher was wired.

This stage establishes the contract: the rebuild is triggered by **`main` advancing**, not by a file watcher, not by a CI hook, not by manual operator action. Every other stage builds on that single trigger point.

### 2.2 Stage 2 — Selective rebuild rule for shared dependencies (PR #285, `84966d09`)

The original rule rebuilt the touched service only. That breaks for `crates/rb-*` changes: a change in `crates/rb-storage-pg` requires rebuilding every service that links it (`control-api`, the workers, projectors, tombstoner — effectively the whole Rust fleet). PR #285 added the "rebuild **all** Rust services when `crates/**`, `Cargo.toml`, `Cargo.lock`, or `proto/**` changes" rule.

The full mapping (canonical source of truth: `docs/dev-stack-auto-rebuild.md` § *Selective rebuild rules*):

| Changed path | Services rebuilt |
|---|---|
| `crates/**`, `Cargo.toml`, `Cargo.lock`, `proto/**` | **All 11 Rust services** (shared dependency change) |
| `services/<name>/**`, `docker/<name>/**` | That specific service only |
| `migrations/**` | `control-api` (+ re-runs `rb-migrations`) |
| `frontend/**`, `docker/frontend/**` | `frontend` |
| `compose/dev.yml`, `compose/full.yml`, `compose/tailscale.yml`, `compose/tailscale.env`, `compose/scripts/**` | All services |
| Anything else (docs, `.github/`, governance, …) | **no rebuild** |

The 11 Rust services as of this ADR: `control-api`, `agent-runner`, `parse-worker`, `typecheck-worker`, `ingest-graph`, `ingest-clone`, `expand-worker`, `embed-worker`, `projector-pg`, `projector-neo4j`, `tombstoner`. **The table in `docs/dev-stack-auto-rebuild.md` is the source of truth** — when a new service joins the workspace it is added there and to the rebuild script's path-mapping logic in the same PR; this ADR is not re-issued for fleet-size churn.

### 2.3 Stage 3 — User systemd service on mars (RUSAA-1537, PR #480, `ba5c8d32`)

Mars is unattended: no operator is logged in continuously, and a foreground watcher dies on logout / reboot. The watcher is therefore packaged as a **user systemd service** with the unit file checked in at `infra/systemd/rustbrain-dev-watch.service`. Two properties make this work:

- `loginctl enable-linger jarnura` is active on mars, so the user manager starts at boot. The service survives logout and machine reboot without root systemd entanglement.
- The unit uses `%h` specifiers (`WorkingDirectory=%h/projects/rustacean`, etc.). The same checked-in unit file works for any operator whose repo is at `~/projects/rustacean` — no per-host editing of unit files, no secret paths embedded in `/etc/systemd/`.

Operator install procedure lives in `docs/dev-stack-auto-rebuild.md` § *Setup on mars*; that doc is the operational counterpart to this ADR.

### 2.4 Stage 4 — Build-SHA provenance (RUSAA-1642 / RUSAA-1644; PRs #566, #568, #570; commits `74272740`, `6025e907`, `af9624c5`)

Selective rebuild plus user systemd gave us *automated* deployment but not *verified* deployment. Two recurring UAT-round failures motivated Stage 4:

- "control-api was rebuilt, but is the running container actually on the merged SHA?"
- "agent-runner and MCP shipped from different `main` SHAs in the same UAT round — did anyone notice?"

`rb-build-info` is a small crate that captures the build-time git SHA via a `build.rs` and exposes it through every service binary. Phase 1 (`74272740`) added the crate and a `GET /health/build` endpoint on `control-api` plus a CI SHA gate so a service binary cannot ship without an embedded SHA. Phase 2 (`6025e907`) rolled the crate out to all 11 services. Phase 3 (`af9624c5`) added agent-runner / MCP pairing — a warn-only mismatch detection so divergent SHAs between paired services are visible in logs without failing the rebuild.

`stack-rebuild:` evidence on Done-gate PRs (per the 2026-05-12 CTO directive recorded in `docs/dev-stack-auto-rebuild.md` § *Done-gate evidence…*) is the user-facing surface of Stage 4: every PR touching `services/control-api/` or `services/agent-runner/` must record the post-merge restart timestamp, which is verified against the running container's `/health/build` SHA.

### 2.5 Stage 5 — Canonical compose-dir invariant (RUSAA-1653, PR #572, `eff8ed51`)

`docker compose` stamps each container with `com.docker.compose.project.working_dir`. Bind-mount paths like `../migrations:/migrations:ro` resolve **relative to that label**, not relative to the running watcher's CWD. A second clone of the repo (e.g. `/tmp/phase2-deploy/rustacean`) that ran `docker compose up` would silently rewrite the label, and when that scratch dir was cleaned up the next container restart would fail on a missing bind source.

The fix is layered defense:

1. **Script-level guard.** `scripts/dev-stack-watch.sh` and `scripts/dev-stack-auto-rebuild.sh` both refuse to run when their own directory or `REPO_ROOT` resolves under `/tmp/`. Exit non-zero with a fatal journal line; systemd's `Restart=on-failure` will keep restarting until the operator points the unit at the canonical clone.
2. **Unit-level guard.** `infra/systemd/rustbrain-dev-watch.service` carries `ConditionPathNotEmpty=…/compose/active-env`. The sentinel file lives only in the canonical clone; a clone without it cannot start the service at all.
3. **Drift detector.** `scripts/check-compose-working-dir.sh` reports `OK` / `DRIFT` per container and exits non-zero on any divergence; `--fix` runs `docker compose up -d --force-recreate` to re-stamp the label.

The invariant: **one canonical clone per host, and the watcher refuses to operate on any other.** Operators may keep additional clones for hotfix work; they may not point the dev-stack at them.

---

## 3. Consequences

**Selective-rebuild rules are part of the contract, not an implementation detail.** The table in `docs/dev-stack-auto-rebuild.md` § *Selective rebuild rules* is referenced from this ADR and from the PR checklist for any new service. Adding a new Rust service requires updating both the rebuild script's path-mapping and the operator doc in the same PR; this ADR is not the source of truth for fleet membership and does not need re-issuing for fleet-size churn.

**Operators can `ssh` into mars and run a rebuild without ambiguity.** The manual escape hatches are documented in `docs/dev-stack-auto-rebuild.md`: `scripts/dev-stack-auto-rebuild.sh [prev_sha new_sha]` for a one-shot rebuild and `--cold-start` for a fully stopped stack. Both go through the same script the watcher uses, so manual and automatic paths produce identical artifacts and identical NDJSON log records.

**`rb-build-info` SHA pairing surfaces drift between control-api and agent-runner/MCP.** Phase 3 is warn-only on purpose — a hard-fail on SHA mismatch would block UAT during the inherent window between rebuild start on service A and rebuild completion on service B. The warn-only signal is consumed by the Done-gate evidence block and by the `dev-deployment` skill's verification step; promoting it to a hard gate is a future decision that requires changing the rebuild orchestration to be cross-service-transactional.

**One watcher per host is sufficient and intended.** Stage 5's canonical-dir invariant is structural: a second watcher cannot coexist on the same host without one of them violating either the script-level `/tmp/` guard or the unit-level `ConditionPathNotEmpty` sentinel. Operators wanting to test watcher changes do so in a non-mars environment.

**Rebuild logs are the durable record.** `~/.local/state/rustbrain/dev-stack-rebuilds.ndjson` is append-only; the most recent N records are queryable via `scripts/dev-stack-auto-rebuild.sh --logs [N]`. The NDJSON schema (`timestamp`, `prev_sha`, `new_sha`, `rebuilt[]`, `result`, `health`, `elapsed_s`, `reason`) is the surface that Done-gate evidence reads from. Changes to the schema are breaking; additive fields only.

**Operator runbook is a separate deliverable.** The recovery procedures (what to do when the watcher is wedged, how to force a cold-start, how to fix working_dir drift, how to read NDJSON logs to triage a stuck UAT round) belong to the C4 stack-rebuild-verify runbook under the Wave-7 docs umbrella ([RUSAA-1664](/RUSAA/issues/RUSAA-1664)), not this ADR. The `dev-deployment` skill covers the mars-specific recovery paths that an agent needs at runtime.

---

## 4. Alternatives considered

### 4.1 Kubernetes operator

Run the dev-stack on a small k8s cluster (k3s, kind, or a single-node managed offering); model the rebuild as a custom operator that reconciles image SHA against `main`.

**Rejected.** Mars is a single host, not a cluster. Adopting k8s for dev-UAT means standing up either a cluster control-plane on mars (overkill, and brittle on a workstation-class box) or a remote cluster (which reintroduces the GHCR-credentials problem we explicitly avoided — see §1). The operational complexity (CRDs, RBAC, image-pull-secrets, ingress) dwarfs the win, and the win itself — declarative reconciliation — is mostly delivered by the existing `docker compose up -d` + selective rebuild loop.

### 4.2 CI-driven push to a registry

Push images from GitHub Actions on every merge to `main`, have mars `docker compose pull` on a cron.

**Rejected** for one structural reason and one operational reason:

- **Structural:** mars has no GHCR write credentials, and the security posture forbids issuing them. The local-build-only invariant (`pull_policy: never` on every custom service in `compose/dev.yml`) is a load-bearing safety net: it makes "did the wrong image get pulled?" impossible to even ask. Removing it to enable registry-driven deploys would erase a property we rely on.
- **Operational:** GitHub Actions runners would need to build the entire Rust workspace on every merge to `main`. The current Cargo cache + selective rebuild on mars is several × faster end-to-end because Cargo's incremental compilation cache is intact between merges; a fresh CI runner is not.

### 4.3 Polling every N minutes from each service

Each service polls a control endpoint to discover "is there a newer SHA?" and self-reschedules a rebuild.

**Rejected.** Cross-process coordination overhead — 11 services racing on the same `git fetch`, the same `docker compose build`, the same migration runner — is strictly worse than one watcher per host. The current design has exactly one writer to the rebuild loop and exactly one writer to the NDJSON log; making 11 writers would require either a lock-and-leader protocol (complex, with a failure mode of "no leader, no rebuilds") or accepting concurrent rebuilds (catastrophic — interleaved `docker compose build` calls produce undefined state). One watcher per host is the minimum-coordination design and it is sufficient.

---

## 5. Open follow-ups (not part of this ADR's acceptance)

- **C4** — stack-rebuild-verify operator runbook covering wedge recovery, cold-start, working_dir drift repair, and NDJSON triage. Separate Wave-7 docs subtask under [RUSAA-1664](/RUSAA/issues/RUSAA-1664).
- **Hard gate on SHA pairing.** Stage 4 Phase 3 is warn-only. Promoting it to a hard gate requires cross-service-transactional rebuild orchestration — a future ADR if/when it becomes load-bearing for UAT correctness rather than diagnostic.
- **Multi-host dev-stack.** If a second UAT host is ever added (geographic redundancy, parallel feature-branch UAT), the canonical-dir invariant generalises to canonical-host-per-environment; the watcher's leader election story is currently "there is only one host" and would need to be redesigned.
