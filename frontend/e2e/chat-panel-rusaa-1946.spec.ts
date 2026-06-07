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
  LIST_MESSAGES_R26_NO_ASS3,
  LIST_MESSAGES_R26_THREE_FULL_PLUS_USER4,
  THREE_TURN_COMPLETED_NO_INPROGRESS_SSE,
  THREE_TURN_MIDSTREAM_NO_INPROGRESS_SSE,
} from "./fixtures/chat-mock-api-cli-restart";

// Regression guard for RUSAA-1946 (R26 UAT fail — PR #728 over-correction).
//
// PR #728 added `liveCompletedCount >= 2 → drop all` in the !firstLiveUser path.
// After a turn's SSE stream completes, the live assistant flips from inProgress=true
// to a completed item. With 2 prior CLI-replays also completed, the count hit ≥ 2
// and the fresh answer was dropped along with the replays, leaving the transcript
// empty for turn-3 until the next send (or reload) triggered a history refetch.
//
// Fix: remove the >= 2 early-return; rely solely on histAssistantSeqs for dedup.
// dedupeAssistantsPerSegment also updated to drop only confirmed replays (startSeq
// in histAssistantSeqs) instead of all completeds when histAssistantSeqs is provided.

test.describe("Chat panel — R26 fixup: fresh completion visible after stream ends [RUSAA-1946]", () => {
  test("turn-3 answer '16' is visible immediately after SSE stream completes (R26 bug)", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // SSE: no user_input events; CLI replays turns 1+2 (same seqs as DB, dropped),
    // then the fresh turn-3 completion arrives (seq=6, not in DB yet → kept).
    await mockChatStream(page, CHAT_SESSION_ID, THREE_TURN_COMPLETED_NO_INPROGRESS_SSE);
    await mockSendChatMessage(page);
    // DB: turns 1+2 fully persisted, turn-3 user stored, assistant NOT yet.
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_R26_NO_ASS3);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // All three user prompts must be visible.
    await expect(page.getByText("what is 2+2")).toBeVisible();
    await expect(page.getByText("what is 4+4")).toBeVisible();
    await expect(page.getByText("what is 8+8")).toBeVisible();

    // All three assistant answers must be visible — including turn-3 ("16") which
    // was the regression: it disappeared until the next send or a page reload.
    await expect(page.getByText("4", { exact: true })).toHaveCount(1);
    await expect(page.getByText("8", { exact: true })).toHaveCount(1);
    await expect(page.getByText("16", { exact: true })).toHaveCount(1);

    // Strict ordering: user1 < ass1(4) < user2 < ass2(8) < user3 < ass3(16).
    const user1Box = await page.getByText("what is 2+2").boundingBox();
    const asst1Box = await page.getByText("4", { exact: true }).boundingBox();
    const user2Box = await page.getByText("what is 4+4").boundingBox();
    const asst2Box = await page.getByText("8", { exact: true }).boundingBox();
    const user3Box = await page.getByText("what is 8+8").boundingBox();
    const asst3Box = await page.getByText("16", { exact: true }).boundingBox();

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
    expect(user3Box!.y).toBeLessThan(asst3Box!.y);
  });

  test("R24 regression guard: CLI replays do not bleed into the next pending slot (!firstLiveUser path)", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // SSE: no user_input events; CLI replays turns 1+2+3 (startSeqs=2,4,6 all in
    // histAssistantSeqs → all dropped). No fresh turn-4 answer yet.
    await mockChatStream(page, CHAT_SESSION_ID, THREE_TURN_COMPLETED_NO_INPROGRESS_SSE);
    await mockSendChatMessage(page);
    // DB: turns 1+2+3 fully persisted, turn-4 user stored, assistant NOT yet.
    // histAssistantSeqs = {2, 4, 6} — the SSE reply seqs all map to DB entries.
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_R26_THREE_FULL_PLUS_USER4);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // All four user prompts must be visible.
    await expect(page.getByText("what is 2+2")).toBeVisible();
    await expect(page.getByText("what is 4+4")).toBeVisible();
    await expect(page.getByText("what is 8+8")).toBeVisible();
    await expect(page.getByText("what is 16+16")).toBeVisible();

    // Each prior answer appears EXACTLY once — no replay bleed.
    await expect(page.getByText("4", { exact: true })).toHaveCount(1);
    await expect(page.getByText("8", { exact: true })).toHaveCount(1);
    await expect(page.getByText("16", { exact: true })).toHaveCount(1);

    // "16" (turn-3's answer) must appear BEFORE "what is 16+16", not after it.
    const asst3Box = await page.getByText("16", { exact: true }).boundingBox();
    const user4Box = await page.getByText("what is 16+16").boundingBox();
    expect(asst3Box).not.toBeNull();
    expect(user4Box).not.toBeNull();
    expect(asst3Box!.y).toBeLessThan(user4Box!.y);
  });

  test("mid-stream replay guard: prior-turn replays stay hidden while turn-3 is still streaming", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // SSE: no user_input events; CLI replays turns 1+2 (dropped by hasLiveInProgress),
    // then turn-3 is still streaming (inProgress=true, startSeq=6).
    await mockChatStream(page, CHAT_SESSION_ID, THREE_TURN_MIDSTREAM_NO_INPROGRESS_SSE);
    await mockSendChatMessage(page);
    // DB: turns 1+2 fully persisted, turn-3 user stored, assistant NOT yet.
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_R26_NO_ASS3);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // All three user prompts must be visible.
    await expect(page.getByText("what is 2+2")).toBeVisible();
    await expect(page.getByText("what is 4+4")).toBeVisible();
    await expect(page.getByText("what is 8+8")).toBeVisible();

    // Turn-3 streaming answer "16" is visible after "what is 8+8".
    await expect(page.getByText("16", { exact: true })).toBeVisible();

    // Prior answers appear EXACTLY once each — no replay contamination.
    await expect(page.getByText("4", { exact: true })).toHaveCount(1);
    await expect(page.getByText("8", { exact: true })).toHaveCount(1);

    // "16" appears AFTER "what is 8+8".
    const user3Box = await page.getByText("what is 8+8").boundingBox();
    const asst3Box = await page.getByText("16", { exact: true }).boundingBox();
    expect(user3Box).not.toBeNull();
    expect(asst3Box).not.toBeNull();
    expect(user3Box!.y).toBeLessThan(asst3Box!.y);
  });
});
