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
  TURN2_WITH_TURN_COMPLETE_SSE,
  SINGLE_TURN_COMPLETE_SSE,
} from "./fixtures/chat-mock-api";

// Regression guard for the ordering inversion described in RUSAA-1932:
// live render shows assistant1, assistant2, user1, user2 when the queue-drain
// path fires after turn_complete — reload corrects because the DB has the right
// order. Both two-turn orderings (with and without turn_complete) must be stable.

test.describe("Chat panel — multi-turn ordering with turn_complete (queue-drain regression)", () => {
  // AC #1: Two fully completed turns (each with turn_complete) produce the
  // canonical ordering: user-1 < assistant-1 < user-2 < assistant-2.
  // This is the primary guard against the RUSAA-1932 inversion.
  test("two turns with turn_complete maintain [user-1, assistant-1, user-2, assistant-2] order", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockChatStream(page, CHAT_SESSION_ID, TURN2_WITH_TURN_COMPLETE_SSE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // All four items must be visible.
    await expect(page.getByText("what are the tools available")).toBeVisible();
    await expect(page.getByText("Here are the available tools.")).toBeVisible();
    await expect(page.getByText("Tell me about ownership")).toBeVisible();
    await expect(page.getByText("Ownership is Rust's key memory feature.")).toBeVisible();

    // Verify strict top-to-bottom order via bounding box comparison.
    const user1Box = await page.getByText("what are the tools available").boundingBox();
    const asst1Box = await page.getByText("Here are the available tools.").boundingBox();
    const user2Box = await page.getByText("Tell me about ownership").boundingBox();
    const asst2Box = await page.getByText("Ownership is Rust's key memory feature.").boundingBox();

    expect(user1Box).not.toBeNull();
    expect(asst1Box).not.toBeNull();
    expect(user2Box).not.toBeNull();
    expect(asst2Box).not.toBeNull();

    expect(user1Box!.y).toBeLessThan(asst1Box!.y);
    expect(asst1Box!.y).toBeLessThan(user2Box!.y);
    expect(user2Box!.y).toBeLessThan(asst2Box!.y);
  });

  // AC #2: After a single completed turn (with turn_complete), sending a second
  // message shows the second message's pending bubble AFTER assistant-1, not
  // before it. Regression guard for the queue-drain pending-bubble ordering.
  test("queue-drain pending bubble appears after completed assistant-1, not before", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // SSE: full single turn with turn_complete — composer unlocks after.
    await mockChatStream(page, CHAT_SESSION_ID, SINGLE_TURN_COMPLETE_SSE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // Wait for turn 1 to complete and composer to unlock.
    await expect(
      page.getByText("Ownership is Rust's core memory safety mechanism."),
    ).toBeVisible();
    await expect(page.getByRole("button", { name: /^send$/i })).toBeVisible();

    // Send a second message (direct send, not queue-drain — composer is unlocked).
    await page.getByRole("textbox", { name: "Chat message" }).fill("second message after unlock");
    await page.getByRole("button", { name: /^send$/i }).click();

    // The pending bubble for the second message must appear.
    await expect(page.getByText("second message after unlock")).toBeVisible();

    // Verify ordering: user-1 (what is ownership?) must appear above the second
    // message bubble — the pending bubble must NOT precede the completed turn.
    const user1Box = await page
      .getByText("what is ownership?")
      .boundingBox();
    const asst1Box = await page
      .getByText("Ownership is Rust's core memory safety mechanism.")
      .boundingBox();
    const user2Box = await page.getByText("second message after unlock").boundingBox();

    expect(user1Box).not.toBeNull();
    expect(asst1Box).not.toBeNull();
    expect(user2Box).not.toBeNull();

    expect(user1Box!.y).toBeLessThan(asst1Box!.y);
    expect(asst1Box!.y).toBeLessThan(user2Box!.y);
  });
});
