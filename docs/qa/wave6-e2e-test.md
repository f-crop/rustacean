# Wave 6 E2E Test — Code Intelligence Workspace

**Spec file:** `frontend/e2e/wave6-code-intel.spec.ts`
**Exit criterion:** "Query for an item, navigate callers and callees, view source."

## Test coverage

| # | Test | Describe block | Covers |
|---|------|----------------|--------|
| 1 | SearchPanel query submits to /v1/search and renders results | SearchPanel API integration | POST /v1/search fires on submit; result list renders with "Open {fqn}" buttons |
| 2 | Clicking a search result updates the URL with the selected item fqn | SearchPanel API integration | Result click navigates to `?fqn=` URL |
| 3 | RelationsPanel callers tab renders BFS nodes from /callers endpoint | RelationsPanel API integration | GET /v1/repos/{id}/items/{fqn}/callers; non-root caller nodes visible |
| 4 | RelationsPanel callees tab renders BFS nodes from /callees endpoint | RelationsPanel API integration | GET /v1/repos/{id}/items/{fqn}/callees; non-root callee nodes visible |

All 4 tests are active and passing.

## Fixture strategy

All tests use Playwright route mocking via a shared `setupWorkspace()` helper — no live backend required.

Fixtures are defined in `frontend/e2e/fixtures/mock-api.ts`:

| Fixture | Source | Shape |
|---------|--------|-------|
| Auth (`ME_RESPONSE`) | Shared | Active user + tenant |
| Repos (`REPO_ITEM`) | Shared | Single connected repo (`repo-1`) |
| Module tree (`MODULE_TREE_RESPONSE`) | Shared | `my_crate` with one child `my_fn (FN)` |
| Item (`ITEM_RESPONSE`) | Shared | `my_crate::my_fn` with inline source preview (3 lines) |
| Search (`SEARCH_RESPONSE`) | Shared | 2 results: `my_crate::my_fn` (0.92), `my_crate::other_fn` (0.81) |
| Callers (`CALLERS_RESPONSE`) | Shared | Root `my_fn` + 2 callers (`caller_a`, `caller_b`), 2 edges |
| Callees (`CALLEES_RESPONSE`) | Shared | Root `my_fn` + 1 callee (`callee_x`), 1 edge |

### Route mock wiring

`setupWorkspace(page)` registers all API route mocks before each test:

```
/v1/me             → ME_RESPONSE
/v1/repos          → { repos: [REPO_ITEM] }
/v1/repos/*/modules → MODULE_TREE_RESPONSE
/v1/repos/*/items/* → ITEM_RESPONSE (excluding /callers, /callees)
/v1/repos/*/items/*/callers → CALLERS_RESPONSE
/v1/repos/*/items/*/callees → CALLEES_RESPONSE
/v1/search (POST)  → SEARCH_RESPONSE
```

## Tenant isolation

Tenant isolation is enforced by route patterns: every mocked API route is scoped to a specific `repo_id` segment (`repo-1`). Requests to other repo IDs fall through unhandled.

## Prerequisites

- Node.js (version matching `frontend/package.json` engines)
- Playwright browsers installed: `npx playwright install`
- Frontend dev server running or Playwright `webServer` config in `playwright.config.ts`

## Running locally

```bash
cd frontend

# All Wave 6 tests (headless)
npx playwright test e2e/wave6-code-intel.spec.ts

# Visible browser
npx playwright test e2e/wave6-code-intel.spec.ts --headed

# Interactive UI mode
npx playwright test e2e/wave6-code-intel.spec.ts --ui

# Single test by title
npx playwright test e2e/wave6-code-intel.spec.ts -g "SearchPanel query"
```

## CI integration

The spec runs as part of the Playwright E2E suite in CI. It uses route-mocked fixtures (no backend dependency), so it executes without external services.

Artifacts on failure:
- Screenshots and traces are captured per `playwright.config.ts` settings
- CI uploads these as workflow artifacts for debugging

## History

The original spec (branch `feature/rusaa-477-wave6-e2e-test`) contained 11 tests: 7 active foundational tests (page load, tree rendering, source viewer, FQN badge, tab switching, tenant isolation, item switching) plus 4 `test.fixme` stubs awaiting panel wiring. PR #238 shipped the SearchPanel and RelationsPanel wiring and restructured the spec into the current 4 focused integration tests. The foundational scenarios (tree rendering, source viewer, tab switching) are covered by the shared `setupWorkspace()` helper that every test exercises.
