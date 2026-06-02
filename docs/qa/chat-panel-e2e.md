# Chat Panel E2E Test Spec

**Epic**: Wave 9 chat panel
**Spec file**: `frontend/e2e/chat-panel.spec.ts`
**Smoke workflow**: `.github/workflows/chat-smoke.yml`

---

## Overview

This document describes the E2E test coverage for the Wave 9 chat panel (S6 deliverable).
The spec exercises the full frontend journey from opening the chat panel through receiving
a streamed response with an MCP tool call, using Playwright route mocks for all API calls.

## Feature flag

The chat panel is gated by the `VITE_FEATURE_CHAT_PANEL=true` build-time env variable.
The smoke CI workflow builds the frontend with this flag set.  The nav link in AppShell
is conditionally rendered.  The `/chat` route itself is always registered; the gate is
UI-only.

## Test cases

### Golden path

| ID | Description | Key assertion |
|----|-------------|---------------|
| GP-1 | Navigate to `/chat` — sidebar and empty state render | `"No sessions yet."` text visible |
| GP-2 | Chat heading present in header | `<h1>Chat</h1>` |
| GP-3 | Click `+ New` in sidebar — composer appears | `aria-label="Chat message"` textarea visible |
| GP-4 | SSE stream renders user message + text response | user bubble + assistant text |
| GP-5 | Tool call block shows `Done` badge after `tool_result` arrives | `aria-label` matches `/list_directory tool call — Done/` |
| GP-6 | Tool call block expands to reveal input + result JSON | `aria-expanded=true`, "Input"/"Result" labels |
| GP-7 | Session ID short-form appears in header | `title=<full-session-id>` |

### Audit row visibility

| ID | Description | Key assertion |
|----|-------------|---------------|
| AV-1 | Activity page `Total audit events` card shows 1 when a chat tool-call audit row is mocked | `SummaryCards` second value `1` |

> **Note**: The frontend `ChatPage` does not directly query `/v1/audit`.  Full audit
> verification (confirming the backend actually persists the row) requires an
> integration test against the live dev stack.  `AV-1` validates that the Activity
> page correctly surfaces audit totals when the backend provides them.

### Error paths

| ID | Description | Key assertion |
|----|-------------|---------------|
| EP-1 | POST `/messages` returns 500 — error alert shown | `role=alert` visible |
| EP-2 | `session.error` SSE event renders as error transcript item | `role=alert` containing "Session timed out" |

## Mock strategy

All API calls are mocked via `page.route()`.  No live backend is required.

| Endpoint | Method | Mock |
|----------|--------|------|
| `/v1/me` | GET | `ME_RESPONSE` (from `fixtures/mock-api.ts`) |
| `/v1/repos` | GET | empty repos list |
| `/v1/chat/sessions` | GET | empty sessions list |
| `/v1/chat/sessions` | POST | `{ session_id: "chat-session-001" }` |
| `/v1/chat/sessions/*/messages` | POST | `{ message_id: "msg-001" }` |
| `/v1/chat/sessions/chat-session-001/events` | GET | SSE body (text/event-stream) |
| `/v1/audit` | GET | `AUDIT_WITH_TOOL_CALL` fixture |

The SSE body (`FULL_EXCHANGE_SSE`) delivers four events in sequence:

```
1. user_input  → "List files in the current directory"
2. tool_use    → name="list_directory", input={path:"."}
3. tool_result → content=["file1.txt","file2.rs"], is_error=false
4. text        → "Here are the files in the current directory."
```

Playwright's `route.fulfill()` returns the complete SSE body immediately when the
`EventSource` connects.

## Quarantine bucket

Flaky chat tests (known SSE timing sensitivity) are moved to
`frontend/e2e/quarantine/chat/`.  The `chat-quarantine` Playwright project runs them
with `retries: 5` in CI (vs `retries: 2` for the main suite).

Run the quarantine project explicitly:

```bash
cd frontend
npx playwright test --project=chat-quarantine
```

The main `chromium` project excludes `**/quarantine/**` via project-level `testIgnore`.

## CI smoke workflow

File: `.github/workflows/chat-smoke.yml`

Triggers: every PR / push to `main` that touches any of:

- `services/chat-runtime/**`
- `services/control-api/src/routes/chat/**`
- `frontend/src/pages/ChatPage.tsx`
- `frontend/src/components/chat/**`
- `frontend/src/api/hooks/useChatSessions.ts`
- `frontend/src/hooks/useChatStream.ts`
- `frontend/e2e/chat-panel.spec.ts`
- `.github/workflows/chat-smoke.yml`

Steps: `npm ci` → `npm run build` (with `VITE_FEATURE_CHAT_PANEL=true`) →
`playwright install --with-deps chromium` → `playwright test e2e/chat-panel.spec.ts`.

## Running locally

```bash
cd frontend

# Build with feature flag on
VITE_FEATURE_CHAT_PANEL=true npm run build

# Preview server (playwright.config uses port 4173)
npm run preview &

# Run chat spec only
npx playwright test e2e/chat-panel.spec.ts --project=chromium

# Open interactive report
npx playwright show-report
```

## Known gaps

- **Live-stack audit assertion**: `AV-1` mocks `/v1/audit`.  A full integration test
  against the dev stack (with a real DB write from the chat gateway) is tracked as a
  follow-on in the UAT acceptance checklist.
- **Multi-session sidebar**: selecting a second session is not yet covered; add to
  quarantine when flakiness profile is understood.
- **Streaming deduplication**: the SSE dedup logic (seq ≤ lastHistSeq) is covered by the
  chat gateway unit tests; frontend dedup E2E coverage can be added once history-join
  support ships.
