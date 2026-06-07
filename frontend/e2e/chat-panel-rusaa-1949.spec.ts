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
  LIST_MESSAGES_R27_TURN2_IN_PROGRESS,
  TWO_TURN_R27_FRESH_FIRST_REPLAY_SECOND_SSE,
  THREE_TURN_COMPLETED_NO_INPROGRESS_SSE,
  LIST_MESSAGES_R26_NO_ASS3,
} from "./fixtures/chat-mock-api-cli-restart";

// Regression guard for RUSAA-1949 (R27 — R26 under-correction).
//
// R26 (PR #730) fixed turn-3 but left turn-2 broken. The root cause:
//   - The CLI restart assigns NEW sequence numbers to replayed responses.
//   - The seq-based filter in the !firstLiveUser path cannot drop these replays.
//   - The SSE stream emits the fresh turn-2 answer FIRST (lower seqs), then the
//     turn-1 replay SECOND (higher seqs).
//   - dedupeAssistantsPerSegment's position-based slice kept the SECOND item
//     (= replay of turn-1's content) instead of the first (= fresh turn-2 answer).
//
// Fix (Approach Y variant): add content-based replay detection in the extraLive
// filter. A live completed assistant whose text matches any historical assistant's
// text verbatim is a replay — drop it regardless of its sequence number.

test.describe("Chat panel — R27 fixup: turn-2 fresh answer shown, not prior-turn replay [RUSAA-1949]", () => {
  test("turn-2 slot shows fresh answer 'Then 2+2=4.' not replayed turn-1 content (AC1)", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // SSE: no user_input events; CLI uses NEW seqs.
    // Fresh turn-2 answer arrives first (seq=100), then turn-1 replay arrives
    // second (seq=102) with the same text as the historical ass-1 row.
    await mockChatStream(page, CHAT_SESSION_ID, TWO_TURN_R27_FRESH_FIRST_REPLAY_SECOND_SSE);
    await mockSendChatMessage(page);
    // DB: turn-1 fully persisted, turn-2 user stored but assistant not yet.
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_R27_TURN2_IN_PROGRESS);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // Both user prompts must be visible.
    await expect(page.getByText("what is 2+@")).toBeVisible();
    await expect(page.getByText("@ is 2")).toBeVisible();

    // Turn-1 assistant ("Did you mean 2+2? That equals 4.") must appear exactly once
    // — from historical, not duplicated from the replay.
    await expect(page.getByText("Did you mean 2+2? That equals 4.")).toHaveCount(1);

    // Turn-2 assistant must show the FRESH answer ("Then 2+2=4."), NOT a duplicate
    // of turn-1's content. This is AC1 — the exact R26 failure mode.
    await expect(page.getByText("Then 2+2=4.")).toBeVisible();

    // Strict ordering: user1 < ass1 < user2 < ass2.
    const user1Box = await page.getByText("what is 2+@").boundingBox();
    const asst1Box = await page.getByText("Did you mean 2+2? That equals 4.").boundingBox();
    const user2Box = await page.getByText("@ is 2").boundingBox();
    const asst2Box = await page.getByText("Then 2+2=4.").boundingBox();

    expect(user1Box).not.toBeNull();
    expect(asst1Box).not.toBeNull();
    expect(user2Box).not.toBeNull();
    expect(asst2Box).not.toBeNull();

    expect(user1Box!.y).toBeLessThan(asst1Box!.y);
    expect(asst1Box!.y).toBeLessThan(user2Box!.y);
    expect(user2Box!.y).toBeLessThan(asst2Box!.y);
  });

  test("turn-1 replay not present after user-2 (no-bleed AC3)", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockChatStream(page, CHAT_SESSION_ID, TWO_TURN_R27_FRESH_FIRST_REPLAY_SECOND_SSE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_R27_TURN2_IN_PROGRESS);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // The replay text "Did you mean 2+2? That equals 4." appears exactly ONCE
    // (from historical turn-1), never duplicated after user-2 (AC3: no bleed).
    await expect(page.getByText("Did you mean 2+2? That equals 4.")).toHaveCount(1);
    // The fresh text "Then 2+2=4." appears exactly once in turn-2 slot.
    await expect(page.getByText("Then 2+2=4.")).toHaveCount(1);
  });

  test("R26 regression guard still passes: turn-3 answer '16' is visible after SSE completes", async ({
    page,
  }) => {
    // Ensure the R26 fix (same-seq CLI replay, fresh at turn-3) still works.
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockChatStream(page, CHAT_SESSION_ID, THREE_TURN_COMPLETED_NO_INPROGRESS_SSE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_R26_NO_ASS3);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    await expect(page.getByText("what is 2+2")).toBeVisible();
    await expect(page.getByText("what is 4+4")).toBeVisible();
    await expect(page.getByText("what is 8+8")).toBeVisible();

    // All three assistant answers visible (R26 core AC).
    await expect(page.getByText("4", { exact: true })).toHaveCount(1);
    await expect(page.getByText("8", { exact: true })).toHaveCount(1);
    await expect(page.getByText("16", { exact: true })).toHaveCount(1);

    // Correct ordering.
    const user3Box = await page.getByText("what is 8+8").boundingBox();
    const asst3Box = await page.getByText("16", { exact: true }).boundingBox();
    expect(user3Box).not.toBeNull();
    expect(asst3Box).not.toBeNull();
    expect(user3Box!.y).toBeLessThan(asst3Box!.y);
  });
});
