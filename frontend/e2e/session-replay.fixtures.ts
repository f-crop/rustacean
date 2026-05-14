import { type Page } from "@playwright/test";

// ---------------------------------------------------------------------------
// Session IDs
// ---------------------------------------------------------------------------

export const SESSION_ID = "aaaabbbb-cccc-dddd-eeee-ffffffffffff";
export const SESSION_ID_RUNNING = "11112222-3333-4444-5555-666677778888";
export const SESSION_ID_PENDING = "22223333-4444-5555-6666-777788889999";

// ---------------------------------------------------------------------------
// Session objects
// ---------------------------------------------------------------------------

export const SESSION_COMPLETED = {
  id: SESSION_ID,
  status: "succeeded",
  runtime_kind: "claude_code",
  created_at: "2025-05-01T10:00:00Z",
  started_at: "2025-05-01T10:00:05Z",
  completed_at: "2025-05-01T10:05:00Z",
  failed_at: null,
  exit_code: 0,
  failure_reason: null,
  tokens_used: 1234,
  token_budget: 100000,
  workspace_path: "tenant-1/aaaabbbb",
  input_prompt_preview: "Build a REST API",
  pid: null,
};

export const SESSION_RUNNING = {
  id: SESSION_ID_RUNNING,
  status: "running",
  runtime_kind: "opencode",
  created_at: "2025-05-01T11:00:00Z",
  started_at: "2025-05-01T11:00:03Z",
  completed_at: null,
  failed_at: null,
  exit_code: null,
  failure_reason: null,
  tokens_used: 500,
  token_budget: 100000,
  workspace_path: "tenant-1/11112222",
  input_prompt_preview: "Fix the auth bug",
  pid: 12345,
};

export const SESSION_PENDING = {
  id: SESSION_ID_PENDING,
  status: "pending",
  runtime_kind: "claude_code",
  created_at: "2025-05-01T12:00:00Z",
  started_at: null,
  completed_at: null,
  failed_at: null,
  exit_code: null,
  failure_reason: null,
  tokens_used: 0,
  token_budget: 100000,
  workspace_path: null,
  input_prompt_preview: "what is 2+2",
  pid: null,
};

// ---------------------------------------------------------------------------
// History event arrays
// ---------------------------------------------------------------------------

// 3 events for the completed session: text, tool_use, tool_result
export const HISTORY_EVENTS_COMPLETED = [
  {
    id: "evt-c001",
    session_id: SESSION_ID,
    tenant_id: "tenant-1",
    event_type: "text",
    sequence: 1,
    created_at: "2025-05-01T10:00:06Z",
    payload: { type: "text", text: "Starting the task." },
  },
  {
    id: "evt-c002",
    session_id: SESSION_ID,
    tenant_id: "tenant-1",
    event_type: "tool_use",
    sequence: 2,
    created_at: "2025-05-01T10:00:07Z",
    payload: {
      type: "tool_use",
      id: "tool-abc",
      name: "Read",
      input: { path: "src/main.rs" },
    },
  },
  {
    id: "evt-c003",
    session_id: SESSION_ID,
    tenant_id: "tenant-1",
    event_type: "tool_result",
    sequence: 3,
    created_at: "2025-05-01T10:00:08Z",
    payload: {
      type: "tool_result",
      tool_use_id: "tool-abc",
      content: "fn main() {}",
      is_error: false,
    },
  },
];

// 3 history events for the running session (seq 1-3); SSE will deliver seq 4
export const HISTORY_EVENTS_RUNNING = [
  {
    id: "evt-r001",
    session_id: SESSION_ID_RUNNING,
    tenant_id: "tenant-1",
    event_type: "text",
    sequence: 1,
    created_at: "2025-05-01T11:00:04Z",
    payload: { type: "text", text: "Analyzing the bug." },
  },
  {
    id: "evt-r002",
    session_id: SESSION_ID_RUNNING,
    tenant_id: "tenant-1",
    event_type: "text",
    sequence: 2,
    created_at: "2025-05-01T11:00:05Z",
    payload: { type: "text", text: "Found the issue." },
  },
  {
    id: "evt-r003",
    session_id: SESSION_ID_RUNNING,
    tenant_id: "tenant-1",
    event_type: "text",
    sequence: 3,
    created_at: "2025-05-01T11:00:06Z",
    payload: { type: "text", text: "Applying the fix." },
  },
];

// ---------------------------------------------------------------------------
// SSE and NDJSON payloads
// ---------------------------------------------------------------------------

// SSE event for the running session: seq 4 (one beyond last history seq)
export const SSE_LIVE_BODY = [
  "event: session.event",
  `data: ${JSON.stringify({
    sequence: 4,
    event_type: "text",
    payload: { type: "text", text: "Live update from SSE" },
  })}`,
  "",
  "",
].join("\n");

// Pre-formatted NDJSON content matching HISTORY_EVENTS_COMPLETED in seq order
export const NDJSON_CONTENT =
  [
    JSON.stringify({
      id: "evt-c001",
      session_id: SESSION_ID,
      sequence: 1,
      event_type: "text",
      payload: { type: "text", text: "Starting the task." },
    }),
    JSON.stringify({
      id: "evt-c002",
      session_id: SESSION_ID,
      sequence: 2,
      event_type: "tool_use",
      payload: { type: "tool_use", id: "tool-abc", name: "Read", input: {} },
    }),
    JSON.stringify({
      id: "evt-c003",
      session_id: SESSION_ID,
      sequence: 3,
      event_type: "tool_result",
      payload: {
        type: "tool_result",
        tool_use_id: "tool-abc",
        content: "fn main() {}",
        is_error: false,
      },
    }),
  ].join("\n") + "\n";

// ---------------------------------------------------------------------------
// List response
// ---------------------------------------------------------------------------

export const SESSION_LIST_RESPONSE = {
  sessions: [
    {
      id: SESSION_ID,
      status: "succeeded",
      runtime_kind: "claude_code",
      created_at: "2025-05-01T10:00:00Z",
      started_at: "2025-05-01T10:00:05Z",
      completed_at: "2025-05-01T10:05:00Z",
      tokens_used: 1234,
      token_budget: 100000,
      workspace_path: "tenant-1/aaaabbbb",
      input_prompt_preview: "Build a REST API",
    },
    {
      id: SESSION_ID_RUNNING,
      status: "running",
      runtime_kind: "opencode",
      created_at: "2025-05-01T11:00:00Z",
      started_at: "2025-05-01T11:00:03Z",
      completed_at: null,
      tokens_used: 500,
      token_budget: 100000,
      workspace_path: "tenant-1/11112222",
      input_prompt_preview: "Fix the auth bug",
    },
  ],
};

// ---------------------------------------------------------------------------
// Mock helpers
// ---------------------------------------------------------------------------

export async function mockSessionList(page: Page): Promise<void> {
  await page.route("**/v1/agents/sessions", (route) => {
    if (route.request().method() === "GET") {
      return route.fulfill({ json: SESSION_LIST_RESPONSE });
    }
    return route.continue();
  });
}

/**
 * Single handler for all /v1/agents/sessions/{SESSION_ID}/** sub-routes:
 * - /log.ndjson         → NDJSON download content
 * - /events/history     → paginated history (one page, no next_seq)
 * - /events             → empty SSE (completed session, no live stream)
 * - (default)           → session detail JSON
 */
export async function mockCompletedSession(page: Page): Promise<void> {
  await page.route(`**/v1/agents/sessions/${SESSION_ID}**`, (route) => {
    const url = route.request().url();
    if (url.includes("/log.ndjson")) {
      return route.fulfill({
        status: 200,
        headers: {
          "Content-Type": "application/x-ndjson",
          "Content-Disposition": `attachment; filename="session-${SESSION_ID}.ndjson"`,
        },
        body: NDJSON_CONTENT,
      });
    }
    if (url.includes("/events/history")) {
      return route.fulfill({
        json: { events: HISTORY_EVENTS_COMPLETED, next_seq: null },
      });
    }
    if (url.includes("/events")) {
      return route.fulfill({
        status: 200,
        headers: {
          "Content-Type": "text/event-stream",
          "Cache-Control": "no-cache",
        },
        body: "",
      });
    }
    return route.fulfill({ json: SESSION_COMPLETED });
  });
}

/**
 * Mock for a pending session with no events yet.
 * - /events/history → empty events
 * - /events         → empty SSE stream (session not yet running)
 * - (default)       → pending session detail JSON
 */
export async function mockPendingSession(page: Page): Promise<void> {
  await page.route(`**/v1/agents/sessions/${SESSION_ID_PENDING}**`, (route) => {
    const url = route.request().url();
    if (url.includes("/events/history")) {
      return route.fulfill({ json: { events: [], next_seq: null } });
    }
    if (url.includes("/events")) {
      return route.fulfill({
        status: 200,
        headers: {
          "Content-Type": "text/event-stream",
          "Cache-Control": "no-cache",
        },
        body: "",
      });
    }
    return route.fulfill({ json: SESSION_PENDING });
  });
}

/**
 * Single handler for all /v1/agents/sessions/{SESSION_ID_RUNNING}/** sub-routes:
 * - /events/history  → 3 history events (seq 1-3)
 * - /events          → SSE body delivering seq 4
 * - (default)        → running session detail JSON
 */
export async function mockRunningSession(page: Page): Promise<void> {
  await page.route(`**/v1/agents/sessions/${SESSION_ID_RUNNING}**`, (route) => {
    const url = route.request().url();
    if (url.includes("/events/history")) {
      return route.fulfill({
        json: { events: HISTORY_EVENTS_RUNNING, next_seq: null },
      });
    }
    if (url.includes("/events")) {
      return route.fulfill({
        status: 200,
        headers: {
          "Content-Type": "text/event-stream",
          "Cache-Control": "no-cache",
        },
        body: SSE_LIVE_BODY,
      });
    }
    return route.fulfill({ json: SESSION_RUNNING });
  });
}
