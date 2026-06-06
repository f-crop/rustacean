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
  LIST_MESSAGES_EMPTY,
  SINGLE_TURN_COMPLETE_SSE,
} from "./fixtures/chat-mock-api";

// ---------------------------------------------------------------------------
// RUSAA-1929: flush pending assistant on turn_complete
// ---------------------------------------------------------------------------

test.describe("Chat panel — turn_complete flushes pending assistant (RUSAA-1929)", () => {
  // AC #1: after a single-turn reply that ends with turn_complete, the composer
  // button reads "Send" (not "Queue"), confirming isComposerLocked === false.
  test("button reads Send and composer is unlocked after turn_complete arrives", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // SSE: user_input → text → turn_complete (fully completed turn)
    await mockChatStream(page, CHAT_SESSION_ID, SINGLE_TURN_COMPLETE_SSE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // Wait for the assistant reply text to confirm the exchange rendered.
    await expect(
      page.getByText("Ownership is Rust's core memory safety mechanism."),
    ).toBeVisible();

    // After turn_complete the composer must not be locked — button shows "Send".
    await expect(page.getByRole("button", { name: /^send$/i })).toBeVisible();

    // The user input field should be interactive.
    await expect(page.getByRole("textbox", { name: "Chat message" })).toBeEnabled();
  });
});
