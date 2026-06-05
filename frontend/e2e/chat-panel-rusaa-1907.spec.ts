import { test, expect } from "@playwright/test";
import {
  mockAuthenticatedSession,
  mockReposList,
  REPOS_EMPTY_RESPONSE,
} from "./fixtures/mock-api";
import {
  mockChatSessionsListAndCreate,
  mockSendChatMessage,
  mockChatStream,
  mockListChatMessages,
  CHAT_SESSION_ID,
  LIST_SESSIONS_ONE,
  TURN1_ASSISTANT_ONLY_SSE,
  LIST_MESSAGES_TURN1_USER_ONLY,
} from "./fixtures/chat-mock-api";

// ---------------------------------------------------------------------------
// Bug C turn-2 edge case — RUSAA-1907: historical user + SSE assistant-only
// ---------------------------------------------------------------------------

test.describe("Chat panel — turn-2 ordering when SSE lacks user_input (RUSAA-1907 edge case)", () => {
  // Regression guard for RUSAA-1907: when the SSE stream joins after the
  // user_input event was emitted (or the server replays only content events),
  // liveItems = [assistant-1(inProgress)] with no user turn.
  // Historical DB provides user-1.  base = [user-1-hist, assistant-1(inProgress)].
  //
  // Without the secondary guard (checking base[candidateSlot-1]?.kind !== "user"),
  // the slot predicate fires (!liveHasUserEcho) and inserts the turn-2 pending
  // bubble at position 1 — between user-1 and assistant-1.
  // The secondary guard prevents this by detecting the user-1-hist pairing.
  test("turn-2: pending bubble appears after assistant-1 when SSE has no user_input and history has user-1", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // SSE has only assistant text — no user_input event (simulates mid-stream join).
    await mockChatStream(page, CHAT_SESSION_ID, TURN1_ASSISTANT_ONLY_SSE);
    await mockSendChatMessage(page);
    // Historical DB has user-1's message but assistant row not yet flushed.
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_TURN1_USER_ONLY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // Both user-1 (from history) and assistant-1 (from SSE) must be visible.
    await expect(page.getByText("what are the tools available")).toBeVisible();
    await expect(page.getByText("Here are the available tools.")).toBeVisible();

    // Send turn-2 optimistically.
    await page.getByRole("textbox", { name: "Chat message" }).fill("explain monad");
    await page.getByRole("button", { name: "Send" }).click();

    await expect(page.getByText("explain monad")).toBeVisible();

    // The turn-2 pending bubble must appear BELOW assistant-1, not before it.
    const pendingBubble = page.getByText("explain monad");
    const assistantContent = page.getByText("Here are the available tools.");

    const pendingBox = await pendingBubble.boundingBox();
    const assistantBox = await assistantContent.boundingBox();

    expect(pendingBox).not.toBeNull();
    expect(assistantBox).not.toBeNull();
    expect(pendingBox!.y).toBeGreaterThan(assistantBox!.y);
  });
});
