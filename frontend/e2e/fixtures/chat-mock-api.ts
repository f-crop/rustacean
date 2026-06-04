import { type Page } from "@playwright/test";

export const CHAT_SESSION_ID = "chat-session-001";

export const CHAT_SESSION_FIXTURE = {
  id: CHAT_SESSION_ID,
  tenant_id: "tenant-1",
  user_id: "user-1",
  runtime: "claude_code" as const,
  status: "active" as const,
  trace_id: "trace-abc123",
  created_at: "2026-06-03T00:00:00Z",
  last_activity_at: "2026-06-03T00:00:01Z",
  ended_at: null,
};

export const LIST_SESSIONS_EMPTY = { sessions: [] };

export const LIST_SESSIONS_ONE = { sessions: [CHAT_SESSION_FIXTURE] };

export const CREATE_SESSION_RESPONSE = { session_id: CHAT_SESSION_ID };

export const SEND_MESSAGE_RESPONSE = { message_id: "msg-001" };

// Full exchange: user_input → tool_use → tool_result → text
export const FULL_EXCHANGE_SSE = [
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "user_input",
    sequence: 1,
    payload: { type: "user_input", text: "List files in the current directory" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "tool_use",
    sequence: 2,
    payload: {
      type: "tool_use",
      id: "tool-001",
      name: "list_directory",
      input: { path: "." },
    },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "tool_result",
    sequence: 3,
    payload: {
      type: "tool_result",
      tool_use_id: "tool-001",
      content: ["file1.txt", "file2.rs"],
      is_error: false,
    },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 4,
    payload: { type: "text", text: "Here are the files in the current directory." },
  })}`,
  "",
  "",
].join("\n");

export const SESSION_ERROR_SSE = [
  "event: session.error",
  `data: ${JSON.stringify({
    error: "timeout",
    status: "504",
    message: "Session timed out",
  })}`,
  "",
  "",
].join("\n");

export const LIST_MESSAGES_TWO_TURNS = {
  messages: [
    {
      id: "msg-001",
      seq: 1,
      role: "user",
      body: "Hello from reload test",
      created_at: "2026-06-03T00:00:00Z",
    },
    {
      id: "msg-002",
      seq: 2,
      role: "assistant",
      body: "Hello back! I remember your message.",
      created_at: "2026-06-03T00:00:01Z",
    },
    {
      id: "msg-003",
      seq: 3,
      role: "user",
      body: "Second message",
      created_at: "2026-06-03T00:00:02Z",
    },
    {
      id: "msg-004",
      seq: 4,
      role: "assistant",
      body: "Got your second message.",
      created_at: "2026-06-03T00:00:03Z",
    },
  ],
  has_more: false,
};

export const LIST_MESSAGES_EMPTY = {
  messages: [] as never[],
  has_more: false,
};

// Two-turn history using "What MCP tools…" prompt (matches AC3 reload assertion).
export const LIST_MESSAGES_MCP_EXCHANGE = {
  messages: [
    {
      id: "msg-mcp-001",
      seq: 1,
      role: "user",
      body: "What MCP tools are available?",
      created_at: "2026-06-03T00:00:00Z",
    },
    {
      id: "msg-mcp-002",
      seq: 2,
      role: "assistant",
      body: "The following MCP tools are registered: bash, read_file, write_file.",
      created_at: "2026-06-03T00:00:01Z",
    },
  ],
  has_more: false,
};

// SSE fixture for a new exchange sent after prior history is already loaded.
// Represents: user sends "How do I use the bash tool?", assistant replies.
export const MID_SEND_SSE = [
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "user_input",
    sequence: 1,
    payload: { type: "user_input", text: "How do I use the bash tool?" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 2,
    payload: { type: "text", text: "You can use the bash tool to run shell commands in the workspace." },
  })}`,
  "",
  "",
].join("\n");

// Tool-use exchange stored in the new JSON content-block body format (post-1896).
// body is a JSON array: [tool_use block, tool_result block, text block].
export const LIST_MESSAGES_WITH_TOOL_USE = {
  messages: [
    {
      id: "msg-tool-u1",
      seq: 1,
      role: "user",
      body: "Search for recent Rust news",
      created_at: "2026-06-04T00:00:00Z",
    },
    {
      id: "msg-tool-a1",
      seq: 2,
      role: "assistant",
      body: JSON.stringify([
        {
          type: "tool_use",
          id: "tu-001",
          name: "mcp__rust_brain__search_demo",
          input: { q: "recent Rust news" },
        },
        {
          type: "tool_result",
          tool_use_id: "tu-001",
          content: "Found 5 results for recent Rust news",
          is_error: false,
        },
        {
          type: "text",
          text: "Here are the recent Rust news results.",
        },
      ]),
      created_at: "2026-06-04T00:00:01Z",
    },
  ],
  has_more: false,
};

// Simulates the SSE echo race: assistant tokens stream for a SECOND turn WITHOUT
// a preceding user_input echo.  buildTranscript accumulates these as an
// in-progress (inProgress: true) assistant item.  Used to test that the
// optimistic pending bubble is slotted BEFORE this in-progress item.
export const IN_PROGRESS_NO_ECHO_SSE = [
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 3,
    payload: { type: "text", text: "I'm analyzing your request now..." },
  })}`,
  "",
  "",
].join("\n");

// Simulates the turn-2 stale-inProgress race: a completed turn-1 exchange
// (user_input + text) where inProgress is never cleared because no subsequent
// user_input arrived.  buildTranscript marks the trailing assistant as
// inProgress: true even though turn-1 is fully done.
// Used to assert that the turn-2 pending bubble is placed AFTER assistant-1,
// NOT slotted before it.
export const TURN1_COMPLETE_STALE_INPROGRESS_SSE = [
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "user_input",
    sequence: 1,
    payload: { type: "user_input", text: "what are the tools available" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 2,
    payload: { type: "text", text: "Here are the available tools." },
  })}`,
  "",
  "",
].join("\n");

// Simulates two completed turns including the turn-2 user_input echo.
// buildTranscript produces: [user-1, assistant-1, user-2, assistant-2(inProgress)]
// Used to verify AC3: after turn-2 echo arrives, ordering is stable and
// no visual reshuffle of items 1 (user-1) and 2 (assistant-1) occurs.
export const TURN2_WITH_ECHO_SSE = [
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "user_input",
    sequence: 1,
    payload: { type: "user_input", text: "what are the tools available" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 2,
    payload: { type: "text", text: "Here are the available tools." },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "user_input",
    sequence: 3,
    payload: { type: "user_input", text: "Tell me about ownership" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 4,
    payload: { type: "text", text: "Ownership is Rust's key memory feature." },
  })}`,
  "",
  "",
].join("\n");

export const AUDIT_WITH_TOOL_CALL = {
  total: 1,
  events: [
    {
      id: "audit-001",
      tenant_id: "tenant-1",
      actor_kind: "user",
      actor_id: "user-1",
      action: "chat.tool_call",
      resource_kind: "chat_message",
      resource_id: CHAT_SESSION_ID,
      ip: "127.0.0.1",
      user_agent: "playwright-test",
      created_at: "2026-06-03T00:00:02Z",
    },
  ],
};

export async function mockChatSessionsList(
  page: Page,
  response: { sessions: unknown[] } = LIST_SESSIONS_EMPTY,
): Promise<void> {
  await page.route("**/v1/chat/sessions", (route) => {
    if (route.request().method() === "GET") {
      return route.fulfill({ json: response });
    }
    return route.continue();
  });
}

export async function mockCreateChatSession(page: Page): Promise<void> {
  await page.route("**/v1/chat/sessions", (route) => {
    if (route.request().method() === "POST") {
      return route.fulfill({ json: CREATE_SESSION_RESPONSE });
    }
    return route.continue();
  });
}

export async function mockChatSessionsListAndCreate(
  page: Page,
  listResponse: { sessions: unknown[] } = LIST_SESSIONS_EMPTY,
): Promise<void> {
  await page.route("**/v1/chat/sessions", (route) => {
    if (route.request().method() === "POST") {
      return route.fulfill({ json: CREATE_SESSION_RESPONSE });
    }
    return route.fulfill({ json: listResponse });
  });
}

export async function mockSendChatMessage(page: Page): Promise<void> {
  await page.route("**/v1/chat/sessions/*/messages", (route) =>
    route.fulfill({ json: SEND_MESSAGE_RESPONSE }),
  );
}

export async function mockListChatMessages(
  page: Page,
  sessionId: string,
  response = LIST_MESSAGES_TWO_TURNS,
): Promise<void> {
  await page.route(`**/v1/chat/sessions/${sessionId}/messages`, (route) => {
    if (route.request().method() === "GET") {
      return route.fulfill({ json: response });
    }
    return route.continue();
  });
}

export async function mockChatStream(
  page: Page,
  sessionId: string,
  sseBody: string,
): Promise<void> {
  await page.route(`**/v1/chat/sessions/${sessionId}/events`, (route) =>
    route.fulfill({
      status: 200,
      headers: {
        "Content-Type": "text/event-stream",
        "Cache-Control": "no-cache",
      },
      body: sseBody,
    }),
  );
}
