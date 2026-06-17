import { test, expect, type Page } from "@playwright/test";
import {
  mockAuthenticatedSession,
  mockReposList,
  REPOS_EMPTY_RESPONSE,
} from "./fixtures/mock-api";
import {
  mockChatStream,
  mockListChatMessages,
  LIST_MESSAGES_EMPTY,
} from "./fixtures/chat-mock-api";

// ---------------------------------------------------------------------------
// RUSAA-2026 regression guard: session switch must never throw a JS error
//
// Root cause: two `.slice()` calls on potentially-undefined values:
//  1. SessionSidebar.tsx — session.id.slice(0,8) when server returns session_id
//  2. MessageThread.tsx  — thinking.slice(0,80) when thinking payload is undefined
//
// Fix: nullish guards at both call sites + Zod parse in listSessions hook.
// ---------------------------------------------------------------------------

const CHAT_URL = "/chat";

const SESSION_A_ID = "aaaaaaaa-0000-0000-0000-000000000001";
const SESSION_B_ID = "bbbbbbbb-0000-0000-0000-000000000002";

const TWO_SESSIONS_RESPONSE = {
  sessions: [
    {
      id: SESSION_A_ID,
      tenant_id: "tenant-1",
      user_id: "user-1",
      runtime: "claude_code",
      status: "active",
      trace_id: "trace-aaa",
      created_at: "2026-06-01T00:00:00Z",
      last_activity_at: "2026-06-01T00:01:00Z",
      ended_at: null,
    },
    {
      id: SESSION_B_ID,
      tenant_id: "tenant-1",
      user_id: "user-1",
      runtime: "claude_code",
      status: "active",
      trace_id: "trace-bbb",
      created_at: "2026-06-02T00:00:00Z",
      last_activity_at: "2026-06-02T00:01:00Z",
      ended_at: null,
    },
  ],
};

// A sessions list where the server uses `session_id` instead of `id` (legacy shape).
// The Zod normaliser in useChatSessions must coerce this without crashing.
const SESSION_ID_SNAKE_CASE_RESPONSE = {
  sessions: [
    {
      session_id: SESSION_A_ID,
      tenant_id: "tenant-1",
      user_id: "user-1",
      runtime: "claude_code",
      status: "active",
      trace_id: "trace-aaa",
      created_at: "2026-06-01T00:00:00Z",
      last_activity_at: "2026-06-01T00:01:00Z",
      ended_at: null,
    },
    {
      session_id: SESSION_B_ID,
      tenant_id: "tenant-1",
      user_id: "user-1",
      runtime: "claude_code",
      status: "active",
      trace_id: "trace-bbb",
      created_at: "2026-06-02T00:00:00Z",
      last_activity_at: "2026-06-02T00:01:00Z",
      ended_at: null,
    },
  ],
};

// SSE stream with a thinking event that has an undefined `thinking` field (malformed).
const SSE_MALFORMED_THINKING = [
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: SESSION_A_ID,
    event_type: "thinking",
    sequence: 1,
    payload: { type: "thinking" /* missing `thinking` field */ },
  })}`,
  "",
  "",
].join("\n");

async function setupTwoSessionsPage(
  page: Page,
  sessionsResponse: { sessions: unknown[] } = TWO_SESSIONS_RESPONSE,
  sseBodyA = "",
  sseBodyB = "",
): Promise<void> {
  await mockAuthenticatedSession(page);
  await mockReposList(page, REPOS_EMPTY_RESPONSE);

  // Mock session list (GET returns both sessions; POST creates)
  await page.route("**/v1/chat/sessions", (route) => {
    if (route.request().method() === "GET") {
      return route.fulfill({ json: sessionsResponse });
    }
    return route.continue();
  });

  await mockListChatMessages(page, SESSION_A_ID, LIST_MESSAGES_EMPTY);
  await mockListChatMessages(page, SESSION_B_ID, LIST_MESSAGES_EMPTY);
  await mockChatStream(page, SESSION_A_ID, sseBodyA);
  await mockChatStream(page, SESSION_B_ID, sseBodyB);
}

test.describe("Chat session switch — no JS crash (RUSAA-2026)", () => {
  test("switching between two sessions 10 times emits no pageerror", async ({ page }) => {
    const pageErrors: string[] = [];
    page.on("pageerror", (err) => pageErrors.push(err.message));

    await setupTwoSessionsPage(page);
    await page.goto(`${CHAT_URL}?sessionId=${SESSION_A_ID}`);
    await page.waitForSelector("aside[aria-label='Chat sessions']");

    const sessionButtons = page.getByRole("list").getByRole("button");
    await expect(sessionButtons).toHaveCount(2);

    // Click between the two sessions 10 times (5 round-trips).
    for (let i = 0; i < 10; i++) {
      const target = i % 2 === 0 ? SESSION_B_ID : SESSION_A_ID;
      await page.goto(`${CHAT_URL}?sessionId=${target}`);
      await page.waitForSelector("aside[aria-label='Chat sessions']");
    }

    expect(pageErrors).toEqual([]);
  });

  test("sessions with snake_case session_id render without crash", async ({ page }) => {
    const pageErrors: string[] = [];
    page.on("pageerror", (err) => pageErrors.push(err.message));

    await setupTwoSessionsPage(page, SESSION_ID_SNAKE_CASE_RESPONSE);
    await page.goto(CHAT_URL);
    await page.waitForSelector("aside[aria-label='Chat sessions']");

    // Sessions should still be rendered (normalised from session_id → id)
    const sessionButtons = page.getByRole("list").getByRole("button");
    await expect(sessionButtons).toHaveCount(2);

    expect(pageErrors).toEqual([]);
  });

  test("malformed thinking SSE event (missing thinking field) does not crash", async ({ page }) => {
    const pageErrors: string[] = [];
    page.on("pageerror", (err) => pageErrors.push(err.message));

    await setupTwoSessionsPage(page, TWO_SESSIONS_RESPONSE, SSE_MALFORMED_THINKING);
    await page.goto(`${CHAT_URL}?sessionId=${SESSION_A_ID}`);
    await page.waitForSelector("aside[aria-label='Chat sessions']");

    // Switch away and back
    await page.goto(`${CHAT_URL}?sessionId=${SESSION_B_ID}`);
    await page.waitForSelector("aside[aria-label='Chat sessions']");
    await page.goto(`${CHAT_URL}?sessionId=${SESSION_A_ID}`);
    await page.waitForSelector("aside[aria-label='Chat sessions']");

    expect(pageErrors).toEqual([]);
  });

  test("no 'Something went wrong' error boundary after session switch", async ({ page }) => {
    await setupTwoSessionsPage(page);
    await page.goto(`${CHAT_URL}?sessionId=${SESSION_A_ID}`);
    await page.waitForSelector("aside[aria-label='Chat sessions']");

    for (let i = 0; i < 6; i++) {
      const target = i % 2 === 0 ? SESSION_B_ID : SESSION_A_ID;
      await page.goto(`${CHAT_URL}?sessionId=${target}`);
      await page.waitForSelector("aside[aria-label='Chat sessions']");
    }

    await expect(page.getByText("Something went wrong")).toHaveCount(0);
    await expect(page.getByText("Cannot read properties of undefined")).toHaveCount(0);
  });
});
