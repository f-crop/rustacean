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
} from "./fixtures/chat-mock-api";
import {
  LIST_MESSAGES_TWO_TURNS_MATH_IN_PROGRESS,
  TWO_TURN_NO_USER_INPUT_REPLAY_SSE,
} from "./fixtures/chat-mock-api-cli-restart";

// Regression guard for RUSAA-1942.
//
// When the CLI restarts mid-session AFTER user_input("8+8") has been processed,
// the SSE stream reconnects without a user_input event — only assistant tokens
// arrive. The !firstLiveUser path in ChatPage concatenates these liveItems onto
// history without per-segment deduplication.  The replayed turn-1 assistant ("8")
// has a new sequence number that passes the startSeq filter and appears as a
// duplicate bubble after "what is 8+8".
//
// Fix: apply dedupeAssistantsPerSegment to the combined base in the !firstLiveUser
// branch so replayed assistants landing in the same user-segment are dropped.

test.describe("Chat panel — no-user-input SSE dedup (!firstLiveUser path) [RUSAA-1942]", () => {
  test("only turn-2's answer (16) appears after 'what is 8+8' — no replayed turn-1 duplicate", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // SSE has NO user_input events: [text("8"), turn_complete, text("16")].
    // "8" is the replayed turn-1 assistant; "16" is the streaming turn-2 answer.
    await mockChatStream(page, CHAT_SESSION_ID, TWO_TURN_NO_USER_INPUT_REPLAY_SSE);
    await mockSendChatMessage(page);
    // DB: turn-1 complete, turn-2 user stored but no asst yet.
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_TWO_TURNS_MATH_IN_PROGRESS);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // Both user bubbles must be visible.
    await expect(page.getByText("what is 4+4")).toBeVisible();
    await expect(page.getByText("what is 8+8")).toBeVisible();

    // Turn-1 answer must appear exactly once.
    await expect(page.getByText("8", { exact: true })).toHaveCount(1);

    // Turn-2 answer must appear exactly once.
    await expect(page.getByText("16", { exact: true })).toHaveCount(1);

    // Strict ordering: user1 < asst1 < user2 < asst2.
    const user1Box = await page.getByText("what is 4+4").boundingBox();
    const asst1Box = await page.getByText("8", { exact: true }).boundingBox();
    const user2Box = await page.getByText("what is 8+8").boundingBox();
    const asst2Box = await page.getByText("16", { exact: true }).boundingBox();

    expect(user1Box).not.toBeNull();
    expect(asst1Box).not.toBeNull();
    expect(user2Box).not.toBeNull();
    expect(asst2Box).not.toBeNull();

    expect(user1Box!.y).toBeLessThan(asst1Box!.y);
    expect(asst1Box!.y).toBeLessThan(user2Box!.y);
    expect(user2Box!.y).toBeLessThan(asst2Box!.y);
  });
});
