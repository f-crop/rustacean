# ADR-010: Tenant-scoped GitHub App install + orphan-reclaim self-heal

**Status:** Accepted
**Date:** 2026-05-25
**Wave:** 7 (consolidation of decisions shipped 2026-05-15 → 2026-05-25)
**Author:** Architect
**Supersedes:** —
**Related:** ADR-005 (initial GitHub App + repo schema, `migrations/control/003_github_and_repos.sql`)

## Source PRs

Every claim in this ADR is traceable to a merged PR on `main`.

| Stage | Paperclip issue | PR | Commit | What shipped |
|-------|-----------------|----|--------|--------------|
| 0 | — (ADR-005) | — | (pre-existing) | `control.github_installations` schema with **globally unique** `github_installation_id` (single-tenant lock invariant). |
| 1 | RUSAA-1473 | #457 | `53c0b1b6` | Removed `installation` / `installation_repositories` from GitHub App manifest `default_events` (manifest creation no longer rejected by GitHub). |
| 2a | RUSAA-1561 | #500 | `ec030b95` | `connect_repo` returns **409 Conflict** with `InstallationForDifferentApp { install_url }` when the installation belongs to a now-deactivated App. |
| 2b | RUSAA-1574 | #511 | `3a44d44c` | `list_available_repos` mirrors the same 409 mapping — token-mint failure → actionable 409 instead of 500. |
| 2c | RUSAA-1513 | #513 | `061001b5` | Frontend `AvailableReposError` surfaces a **Re-install** button on 409 (previously only on 404). |
| 2d | RUSAA-1515 | #515 | `7a938824` | Re-install button always goes through the `useGithubInstallUrl()` state-token flow (raw `install_url` from the 409 body lacked the `?state=` parameter the callback requires). |
| 3 | RUSAA-1661 | #574 | `8edd18f6` | Install callback attempts an **atomic orphan-reclaim** before redirecting to the conflict page. Self-heals fresh-tenant collisions against abandoned-tenant rows. |

---

## 1. Context

`control.github_installations` (ADR-005, `migrations/control/003_github_and_repos.sql:8`) carries a database-enforced invariant:

```sql
github_installation_id BIGINT NOT NULL UNIQUE
```

This `UNIQUE` constraint is **global**, not scoped by tenant. The implication is the **single-tenant lock**: one GitHub installation can be owned by at most one Rustacean tenant at a time. Two tenants cannot connect the same GitHub App install simultaneously.

The lock is a feature, not an accident. It mirrors GitHub's own per-install permission model — a GitHub App install grants repo-level access to a specific account/org, and conflating two Rustacean tenants behind one install would let tenant A see (and ingest) tenant B's GitHub repos through the shared installation token.

The cost of the lock is that **fresh tenants whose owner had previously installed the same GitHub App** hit an install-callback collision. The original behaviour was:

1. User on tenant A installs the App → row created with `tenant_id = A`.
2. Tenant A is abandoned (account deleted, environment torn down, dev-stack rebuilt).
3. Same user creates tenant B → re-runs `/v1/github/install` → install callback fires with the same `github_installation_id`.
4. `INSERT INTO control.github_installations (…, tenant_id = B, github_installation_id = X)` fails on the `UNIQUE` constraint.
5. The user sees a generic conflict page; the only escape hatch was a manual SQL rebind:

   ```sql
   UPDATE control.github_installations
   SET tenant_id = '<new>', deleted_at = NULL, suspended_at = NULL
   WHERE github_installation_id = X;
   ```

   Operators ran this by hand on mars; there was no operator UI and no audit trail.

In parallel, two adjacent bugs surfaced on the `/repos` endpoints when an install row pointed at a now-deactivated GitHub App (different but related orphan condition): token-mint against GitHub's `access_tokens` endpoint returned 404, and the control-api fell through to a misleading 422 (`repo_not_accessible`) or 500 instead of telling the user "re-install the current App."

Wave 7 ratified a three-stage evolution that (a) hardens the install path against the App-manifest regression that previously made every install attempt fail, (b) turns orphan-installation token failures into actionable 409s with a specific UI affordance, and (c) self-heals the cross-tenant collision case in the install callback itself, preserving the single-tenant lock without requiring manual operator intervention.

---

## 2. Decision

We keep the global `UNIQUE(github_installation_id)` lock and self-heal **only** the safe sub-cases. The decision is layered, not monolithic — each stage shipped independently and is documented here as one consolidated story.

### 2.1 Stage 1 — Manifest hygiene (RUSAA-1473, PR #457, `53c0b1b6`)

`installation` and `installation_repositories` are **not** valid `default_events` values in a GitHub App manifest. The original manifest builder included them and GitHub rejected the rendered manifest with `Default events unsupported`, blocking every install attempt at the manifest-creation step.

**Decision:** clear those entries from `default_events`. The events are still delivered to the App via webhook URLs (configured separately); `default_events` only governs the *pre-checked* event boxes on the GitHub App creation form.

This stage unblocks the install flow; without it, none of the later stages are observable.

### 2.2 Stage 2 — Actionable 409 on token-mint failures (RUSAA-1561 / RUSAA-1574; PRs #500, #511; commits `ec030b95`, `3a44d44c`)

When an installation row in `control.github_installations` references an installation that GitHub no longer accepts tokens for (most often: the App that minted it has been deactivated and replaced), the **token-mint** step against `https://api.github.com/app/installations/{id}/access_tokens` returns 404 or 401. The user-visible failure mode was a 500 or a misleading 422.

**Decision:** detect token-mint failure as a distinct error and map it to `AppError::InstallationForDifferentApp { install_url }` → HTTP **409 Conflict** with a structured body:

```jsonc
{
  "error": {
    "code": "installation_for_different_app",
    "message": "…",
    "install_url": "https://github.com/apps/<active-slug>/installations/new"
  }
}
```

`install_url` is built from `control.github_app_config.slug` (the **currently active** App), not from the orphan row. Both REST surfaces — `connect_repo` and `list_available_repos` — return the same 409 shape so frontend error-handling is uniform.

Frontend (RUSAA-1513 / RUSAA-1515, PRs #513 / #515): the `AvailableReposError` branch that already showed a "Re-install" affordance on 404 widens to also cover 409. The button **always** routes through `useGithubInstallUrl()` so the install request carries a fresh `?state=` token; the 409 body's `install_url` is a plain GitHub URL without state and was a one-day false-start (see PR #515's commit message — without `state` the callback short-circuits to `/repos` without creating a row, leaving the user in the same broken state).

### 2.3 Stage 3 — Server-side orphan-reclaim (RUSAA-1661, PR #574, `8edd18f6`)

The install callback (`services/control-api/src/routes/github/install.rs`) now attempts a single **atomic CTE reclaim** before redirecting to the conflict page.

**Reclaim is permitted iff** all of the following hold for the existing row:

- It is soft-deleted (`gi.deleted_at IS NOT NULL`), **or** its owning tenant is in a terminal state (`tenants.deleted_at IS NOT NULL` **or** `tenants.status IN ('deleting', 'deleted')`); **and**
- No **active** repos are still linked: `NOT EXISTS (SELECT 1 FROM control.repos r WHERE r.installation_id = gi.id AND r.archived_at IS NULL)`.

If those conditions are met, a single statement transfers ownership:

```sql
WITH reclaimable AS (
  SELECT gi.id, gi.tenant_id AS prior_tenant_id
  FROM   control.github_installations gi
  JOIN   control.tenants t ON t.id = gi.tenant_id
  WHERE  gi.github_installation_id = $1
    AND  gi.tenant_id <> $2
    AND  ( gi.deleted_at IS NOT NULL
        OR t.deleted_at IS NOT NULL
        OR t.status IN ('deleting','deleted') )
    AND  NOT EXISTS (
           SELECT 1 FROM control.repos r
           WHERE  r.installation_id = gi.id AND r.archived_at IS NULL )
)
UPDATE control.github_installations gi
SET    tenant_id     = $2,
       account_login = $3,
       account_type  = $4,
       account_id    = $5,
       deleted_at    = NULL,
       suspended_at  = NULL
FROM   reclaimable
WHERE  gi.id = reclaimable.id
RETURNING gi.id, reclaimable.prior_tenant_id;
```

Outcomes:

- **Reclaimed** → 302 to `/repos?install=success&installation_uuid=…&account_login=…` (identical to a fresh-install success). A `tracing::warn!` records `requesting_tenant`, `prior_tenant`, `installation_id`, `installation_uuid`, and `account` so the reclaim is auditable in logs.
- **Active owner** (collision with a tenant that is not deleted/deleting) → 302 to `/repos?install=conflict&reason=active`. Frontend renders a more specific toast than the generic "conflict" message and the manual SQL rebind path remains the operator escape hatch for **genuine** cross-account conflicts (different GitHub identity, both tenants live).

Integration tests in `services/control-api/tests/integration_github_install_conflict.rs` cover the three branches:

1. Install row soft-deleted → reclaim succeeds.
2. Owner tenant is in a terminal state → reclaim succeeds.
3. Active repos still linked → reclaim blocked; user lands on the conflict page.

**No schema migration.** The global `UNIQUE(github_installation_id)` constraint is preserved verbatim — the reclaim works by `UPDATE`-ing the row's `tenant_id`, not by inserting a second row.

---

## 3. Consequences

**Single-tenant lock preserved.** The database invariant is unchanged. Every reclaim is a tenant-ownership *transfer* on a single row; at no point do two tenants share an installation.

**Self-heal is opt-in by safety, not by feature flag.** The reclaim only fires for orphans whose prior owner is verifiably gone *and* whose repos are no longer active. The set of conditions is conservative on purpose — false positives would silently re-tenant a live install, which is the failure mode the single-tenant lock exists to prevent. The cost of being conservative is that some genuinely-stale rows (e.g. tenant is in `'suspended'` rather than `'deleting'`) still go through the manual path.

**Manual SQL rebind retained as the operator escape hatch.** For genuine cross-account conflicts where both tenants are live, the only legitimate resolution is human judgement: which tenant *should* own this install? The 409-with-`reason=active` redirect makes the case visible; the operator runs the rebind. No automated reclaim is correct here.

**Frontend Re-install button is now load-bearing.** The state-token-only flow (RUSAA-1515) is the single source of truth for re-install attempts. If a future PR introduces another 4xx → install-url branch, it **must** route through `useGithubInstallUrl()`; routing directly to the 409 body's `install_url` is the regression that RUSAA-1515 fixed and the diff must not be reverted.

**Audit trail lives in logs, not the schema.** The reclaim writes a `tracing::warn!` with `prior_tenant`, `requesting_tenant`, and `installation_uuid`. There is **no** persistent reclaim-history table today. If compliance later requires a queryable audit record (e.g. "show every cross-tenant install transfer for tenant X in the last 90 days"), that is a follow-up schema addition, not a redo of this decision.

**Operations runbook is a separate deliverable.** The remaining manual-rebind procedure (when and how to run the SQL, what to verify on the active tenant after rebind) belongs to the C4 runbook under the Wave-7 docs umbrella ([RUSAA-1664](/RUSAA/issues/RUSAA-1664)), not this ADR.

---

## 4. Alternatives considered

### 4.1 Global multi-tenant install model

Let one GitHub installation back multiple Rustacean tenants by dropping `UNIQUE(github_installation_id)` and adding a per-tenant join row.

**Rejected** because it changes GitHub's own permission model. A GitHub install grants repo-level access; if tenant A and tenant B share an installation, tenant A can mint a token (via the App's private key + installation id) that reads tenant B's selected repos. The Rustacean-side tenant scoping cannot stop this — the leak is below our control plane. Preserving the single-tenant lock keeps GitHub's permission boundary aligned with Rustacean's tenant boundary.

### 4.2 Per-tenant GitHub App registration

Have each tenant register its **own** GitHub App (manifest flow per tenant), so installs are naturally tenant-scoped at the App level.

**Rejected** on two grounds:

- **Operational burden.** Each new GitHub App requires manifest creation, private-key storage, webhook URL registration, secret rotation. Multiplying this by tenant count is a step-function increase in onboarding friction and secret-management surface area.
- **GitHub rate limits.** App-creation and App-API rate limits are per-account, and the App-creation manifest flow involves user-attended GitHub UI. Tenants with many sub-organisations would hit either the rate limit or user-experience walls during onboarding.

The single-App + per-tenant-install model is the GitHub-recommended shape for multi-tenant integrators. We keep it.

### 4.3 Asynchronous reclaim worker

Detect orphan-installation rows in a background job (e.g. nightly sweep), proactively soft-delete them, and let the next install attempt fall through to the standard insert path.

**Rejected** as more complex than the synchronous CTE in §2.3 with no observable benefit. The synchronous reclaim runs during a callback that is already a user-facing redirect, atomically updates exactly one row, and exits in a single statement. A background worker would (a) need its own correctness story for "is this row really orphaned?", (b) add a periodic cron surface, and (c) introduce a window where an orphan row exists but is not yet swept — during which the user still sees the old conflict page. The synchronous path is strictly better.

---

## 5. Open follow-ups (not part of this ADR's acceptance)

- **C4** — operator runbook for the manual rebind path on `reason=active` collisions: [`docs/runbooks/github-install-rebind.md`](../runbooks/github-install-rebind.md) (Wave-7 docs subtask under [RUSAA-1664](/RUSAA/issues/RUSAA-1664)).
- **C5** — `docs/architecture.md` does not currently have a dedicated GitHub-integration section. The architecture-doc refresh under C5 is the right surface to add the cross-link to this ADR; doing it here would create a one-line section with no surrounding context. Deferred per the issue's acceptance-criteria escape clause.
- **Persistent reclaim audit table** — see §3. Out of scope for Wave 7; revisit when/if compliance requirements name a queryable surface.
