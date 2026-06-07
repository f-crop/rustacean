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
  LIST_MESSAGES_THREE_TURNS_IN_PROGRESS,
  THREE_TURN_REPLAY_SSE,
} from "./fixtures/chat-mock-api";

// Regression guard for RUSAA-1857: when the CLI restarts mid-session and SSE
// delivers historical assistant responses after the current user_input event
// (firstLiveUser !== null path), only the LAST assistant per user-input segment
// must appear in the transcript — not the full replay from prior turns.
//
// Symptom (pre-fix): after sending "what is 8+8", the response area showed
// three separate assistant bubbles ("4", "14", "16") instead of just "16".
// The SSE stream replayed the full history after user_input("8+8"), and
// dedupeAssistantsPerSegment was missing, so liveItems was used as-is.

test.describe("Chat panel — CLI restart replay dedup (RUSAA-1857)", () => {
  test("only the last assistant response is shown after the current user message", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // SSE: 3 user_input turns with the CLI replaying full history after turn 3.
    // Turn 3 segment in SSE: user_input("8+8"), text("4"), tc, text("14"), tc, text("16").
    // Only "16" (the last assistant in segment 3) must render after "what is 8+8".
    await mockChatStream(page, CHAT_SESSION_ID, THREE_TURN_REPLAY_SSE);
    await mockSendChatMessage(page);
    // History: turns 1+2 complete, user-3 in-flight (no asst-3 in DB yet).
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_THREE_TURNS_IN_PROGRESS);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // All 3 user bubbles must be visible.
    await expect(page.getByText("what is 2+2")).toBeVisible();
    await expect(page.getByText("what is 7+7")).toBeVisible();
    await expect(page.getByText("what is 8+8")).toBeVisible();

    // The correct per-turn assistant responses must be visible.
    await expect(page.getByText("4")).toBeVisible();
    await expect(page.getByText("14")).toBeVisible();
    await expect(page.getByText("16")).toBeVisible();

    // "4" and "14" must each appear exactly ONCE — as the responses to turns 1 and 2,
    // not as extra bubbles after "what is 8+8".
    await expect(page.getByText("4")).toHaveCount(1);
    await expect(page.getByText("14")).toHaveCount(1);
    await expect(page.getByText("16")).toHaveCount(1);

    // Strict ordering: user1 < asst1 < user2 < asst2 < user3 < asst3.
    const user1Box = await page.getByText("what is 2+2").boundingBox();
    const asst1Box = await page.getByText("4").boundingBox();
    const user2Box = await page.getByText("what is 7+7").boundingBox();
    const asst2Box = await page.getByText("14").boundingBox();
    const user3Box = await page.getByText("what is 8+8").boundingBox();
    const asst3Box = await page.getByText("16").boundingBox();

    expect(user1Box).not.toBeNull();
    expect(asst1Box).not.toBeNull();
    expect(user2Box).not.toBeNull();
    expect(asst2Box).not.toBeNull();
    expect(user3Box).not.toBeNull();
    expect(asst3Box).not.toBeNull();

    expect(user1Box!.y).toBeLessThan(asst1Box!.y);
    expect(asst1Box!.y).toBeLessThan(user2Box!.y);
    expect(user2Box!.y).toBeLessThan(asst2Box!.y);
    expect(asst2Box!.y).toBeLessThan(user3Box!.y);
    // The turn-3 assistant ("16") must appear AFTER "what is 8+8", not before it.
    expect(user3Box!.y).toBeLessThan(asst3Box!.y);
  });
});
