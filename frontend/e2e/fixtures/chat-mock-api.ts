import { type Page } from "@playwright/test";

export * from "./chat-fixtures";
export * from "./chat-fixtures-v2";
import {
  CHAT_SESSION_ID,
  CREATE_SESSION_RESPONSE,
  LIST_MESSAGES_TWO_TURNS,
  SEND_MESSAGE_RESPONSE,
} from "./chat-fixtures";

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
  response: { sessions: unknown[] } = { sessions: [] },
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
  listResponse: { sessions: unknown[] } = { sessions: [] },
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
