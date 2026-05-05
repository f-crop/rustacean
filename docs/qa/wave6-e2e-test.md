# Wave 6 E2E Test — Code Intelligence Workspace

**Issue:** RUSAA-477
**File:** `frontend/e2e/wave6-code-intel.spec.ts`
**Exit criterion:** "Query for an item, navigate callers and callees, view source."

## Test coverage

| # | Test | Status | Covers |
|---|------|--------|--------|
| 1 | code workspace page loads with module tree | **active** | Route `/repos/$repoId/code` boots, module tree renders |
| 2 | module tree renders nested items with kind labels | **active** | Tree shows modules, functions, structs by name |
| 3 | clicking module tree item loads source viewer | **active** | Tree click → `?fqn=` navigation → source with line numbers |
| 4 | source viewer shows FQN and kind badge | **active** | FQN, kind (`FN`/`STRUCT`), file path, line range |
| 5 | side panel has search and relations tabs | **active** | Tab switching between search/relations panels |
| 6 | no cross-tenant leakage | **active** | All API requests scoped to correct `repo_id` |
| 7 | selecting different tree items updates source viewer | **active** | Navigating between items updates source pane |
| 8 | search panel: query → click result → item | **fixme** | Requires `SearchPanel` implementation (POST /v1/search) |
| 9 | relations panel: callers BFS graph | **fixme** | Requires `RelationsPanel` wiring (GET /v1/.../callers) |
| 10 | relations panel: callees BFS graph | **fixme** | Requires `RelationsPanel` wiring (GET /v1/.../callees) |
| 11 | full exit-criteria flow | **fixme** | End-to-end: search → item → callers → callees → source |

## Fixture strategy

All tests use Playwright route mocking with deterministic fixture data — no live backend required. Fixtures cover:

- **Auth:** `ME_RESPONSE` (shared from `fixtures/mock-api.ts`)
- **Repos:** `REPO_ITEM` (shared)
- **Module tree:** Nested tree with `my_crate → config → {load_config (FN), Config (STRUCT)}` + `main (FN)`
- **Item detail:** `load_config` with inline source preview (6 lines, lines 23–28)
- **Callers:** 2 direct callers (`main`, `cli::run`)
- **Callees:** 2 direct callees (`std::fs::read_to_string`, `toml::from_str`)

## Tenant isolation assertion

Test #6 captures all outgoing API URLs and asserts every request includes the correct `repo_id` segment, preventing cross-tenant data leakage.

## Activating fixme tests

When `SearchPanel` and `RelationsPanel` are wired to the backend:

1. Remove `test.fixme` from tests 8–11
2. Verify the UI selectors match the implemented components
3. Run: `npx playwright test e2e/wave6-code-intel.spec.ts`

## Running

```bash
cd frontend
npx playwright test e2e/wave6-code-intel.spec.ts          # headless
npx playwright test e2e/wave6-code-intel.spec.ts --headed  # visible browser
npx playwright test e2e/wave6-code-intel.spec.ts --ui      # interactive UI mode
```
