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
  LIST_MESSAGES_EMPTY,
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

// ---------------------------------------------------------------------------
// Bug — RUSAA-1912: 3-turn rapid-send ordering inverts when history is empty
// ---------------------------------------------------------------------------

test.describe("Chat panel — 3-turn rapid-send ordering (RUSAA-1912)", () => {
  // Regression guard for RUSAA-1912: when the user sends 3 messages in rapid
  // succession before SSE echoes any user_input, liveItems = [assistant-1(inProgress)]
  // and history is empty.  candidateSlot = 0, base[-1] is undefined so the
  // secondary guard `base[candidateSlot-1]?.kind !== "user"` is true — without
  // the multi-pending fix all 3 items slot BEFORE the in-progress assistant,
  // producing [user-1, user-2, user-3, assistant-1] instead of
  // [user-1, assistant-1, user-2, user-3].
  test("turn-2 and turn-3 pending bubbles appear AFTER in-progress assistant when history is empty and SSE has no user_input", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // SSE has only assistant text — no user_input events (stream joined mid-turn).
    await mockChatStream(page, CHAT_SESSION_ID, TURN1_ASSISTANT_ONLY_SSE);
    await mockSendChatMessage(page);
    // History is empty — DB cache hasn't refreshed yet.
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // The in-progress assistant text must be visible from the SSE stream.
    await expect(page.getByText("Here are the available tools.")).toBeVisible();

    const composer = page.getByRole("textbox", { name: "Chat message" });
    const sendButton = page.getByRole("button", { name: "Send" });

    // Send 3 messages in rapid succession (no waiting for echoes between sends).
    await composer.fill("what are the tools available");
    await sendButton.click();

    await composer.fill("what is monad");
    await sendButton.click();

    await composer.fill("what is lift doing");
    await sendButton.click();

    // All 3 pending bubbles must be visible.
    await expect(page.getByText("what are the tools available")).toBeVisible();
    await expect(page.getByText("what is monad")).toBeVisible();
    await expect(page.getByText("what is lift doing")).toBeVisible();

    const assistantContent = page.getByText("Here are the available tools.");
    const bubble1 = page.getByText("what are the tools available");
    const bubble2 = page.getByText("what is monad");
    const bubble3 = page.getByText("what is lift doing");

    const [assistantBox, box1, box2, box3] = await Promise.all([
      assistantContent.boundingBox(),
      bubble1.boundingBox(),
      bubble2.boundingBox(),
      bubble3.boundingBox(),
    ]);

    expect(assistantBox).not.toBeNull();
    expect(box1).not.toBeNull();
    expect(box2).not.toBeNull();
    expect(box3).not.toBeNull();

    // user-1 (trigger message) must appear ABOVE in-progress assistant.
    expect(box1!.y).toBeLessThan(assistantBox!.y);
    // user-2 and user-3 (subsequent sends) must appear BELOW assistant.
    expect(box2!.y).toBeGreaterThan(assistantBox!.y);
    expect(box3!.y).toBeGreaterThan(assistantBox!.y);
    // user-3 must appear below user-2.
    expect(box3!.y).toBeGreaterThan(box2!.y);
  });
});
