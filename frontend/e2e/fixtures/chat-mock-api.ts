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
  response = LIST_SESSIONS_EMPTY,
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
  listResponse = LIST_SESSIONS_EMPTY,
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
