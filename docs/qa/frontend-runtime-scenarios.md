# Frontend Runtime Scenarios

Canonical list of FE runtime scenarios that must be covered by the gating Playwright spec set.
Engineers add rows when new scenarios are introduced. QA flags `GAP` rows as soft caveats in QA reports.

**Last reviewed:** 2026-04-28  
**Owner:** QA Engineer

---

## Scenarios

| ID | Behaviour | WCAG ref | Status | Spec file | Owner |
|----|-----------|----------|--------|-----------|-------|
| FE-S-001 | Unauthenticated `GET /` redirects to `/login` and `#root` is visible | — | READY | `frontend/e2e/smoke.spec.ts` | QA |
| FE-S-002 | `/login` passes axe WCAG 2.1 AA scan (0 violations) | WCAG 2.1 AA | READY | `frontend/e2e/axe-dispatch.spec.ts` | QA |
| FE-S-003 | `/signup` passes axe WCAG 2.1 AA scan (0 violations) | WCAG 2.1 AA | READY | `frontend/e2e/axe-dispatch.spec.ts` | QA |
| FE-S-004 | `/forgot-password` passes axe WCAG 2.1 AA scan (0 violations) | WCAG 2.1 AA | READY | `frontend/e2e/axe-dispatch.spec.ts` | QA |
| FE-S-005 | `/repos` passes axe WCAG 2.1 AA scan (0 violations) | WCAG 2.1 AA | READY | `frontend/e2e/axe-dispatch.spec.ts` | QA |
| FE-S-006 | All gated routes have ≥ 1 focusable element reachable via Tab | WCAG 2.1 SC 2.1.1 | READY | `frontend/e2e/axe-dispatch.spec.ts` | QA |
| FE-S-007 | Nav link active-state meets WCAG 2.1 AA colour-contrast ratio (≥ 4.5 : 1) | WCAG 2.1 SC 1.4.3 | READY | `frontend/e2e/axe-dispatch.spec.ts` | QA |
| FE-S-008 | GitHub App install callback (`GET /repos?install=success`) shows success state without error banner | — | GAP | — | FE engineer |
| FE-S-009 | Authenticated user navigating to `/repos` sees populated repo list (or empty-state CTA) | — | GAP | — | FE engineer |
| FE-S-010 | `/reset-password` passes axe WCAG 2.1 AA scan (0 violations) | WCAG 2.1 AA | READY | `frontend/e2e/axe-dispatch.spec.ts` | QA |

---

## Status legend

| Status | Meaning |
|--------|---------|
| `READY` | Scenario is covered by a spec that runs in the gating CI job |
| `GAP` | Scenario is known but has no covering spec yet; QA flags this as a soft caveat in QA reports |
| `WONT_COVER` | Deliberately excluded (note reason in the row) |

---

## Gap tracking

Gaps FE-S-008 and FE-S-009 require authenticated-user test fixtures that are not yet wired into the E2E harness. Both should be addressed after the session/cookie injection helper lands.

QA will update this file when new specs are merged and will flag any new `GAP` rows in QA report comments.
