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
  LIST_MESSAGES_FOUR_TURNS_IN_PROGRESS,
  FOUR_TURN_CLI_REPLAY_SSE_NO_INPROGRESS,
  FOUR_TURN_CLI_REPLAY_SSE_WITH_INPROGRESS,
} from "./fixtures/chat-mock-api-cli-restart";

// Regression guard for RUSAA-1944 (R24 UAT fail).
//
// After the user sends turn-4 ("what is 100+100"), the CLI replays the full prior
// conversation history before streaming the real answer. In the firstLiveUser path,
// dedupeAssistantsPerSegment kept the LAST completed replay ("32") as turn-4's
// assistant bubble — causing the prior turn's answer to appear until SSE resolved.
//
// Fix: when a user-segment contains 2+ completed assistants with no in-progress one,
// drop them all (they are CLI replays; the real answer hasn't started streaming yet).

test.describe("Chat panel — pending-turn CLI-replay dedup [RUSAA-1944]", () => {
  test("turn-4 assistant slot is empty (no replayed text) while awaiting real answer", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // SSE: 3 complete turns + user_input(turn-4) + CLI replays of turns 1-3 (no real "200" yet).
    await mockChatStream(page, CHAT_SESSION_ID, FOUR_TURN_CLI_REPLAY_SSE_NO_INPROGRESS);
    await mockSendChatMessage(page);
    // DB: 3 complete turns + turn-4 user stored, no assistant yet.
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_FOUR_TURNS_IN_PROGRESS);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // All four user prompts must be visible.
    await expect(page.getByText("what is 2+2")).toBeVisible();
    await expect(page.getByText("what is 8+8")).toBeVisible();
    await expect(page.getByText("what is 16+16")).toBeVisible();
    await expect(page.getByText("what is 100+100")).toBeVisible();

    // Each prior answer appears EXACTLY once (no replay duplicates).
    await expect(page.getByText("4", { exact: true })).toHaveCount(1);
    await expect(page.getByText("16", { exact: true })).toHaveCount(1);
    await expect(page.getByText("32", { exact: true })).toHaveCount(1);

    // "32" must appear BEFORE "what is 100+100" — it belongs to turn-3, not turn-4.
    const asst3Box = await page.getByText("32", { exact: true }).boundingBox();
    const user4Box = await page.getByText("what is 100+100").boundingBox();
    expect(asst3Box).not.toBeNull();
    expect(user4Box).not.toBeNull();
    expect(asst3Box!.y).toBeLessThan(user4Box!.y);

    // Strict ordering: user1 < asst1(4) < user2 < asst2(16) < user3 < asst3(32) < user4.
    const user1Box = await page.getByText("what is 2+2").boundingBox();
    const asst1Box = await page.getByText("4", { exact: true }).boundingBox();
    const user2Box = await page.getByText("what is 8+8").boundingBox();
    const asst2Box = await page.getByText("16", { exact: true }).boundingBox();
    const user3Box = await page.getByText("what is 16+16").boundingBox();

    expect(user1Box!.y).toBeLessThan(asst1Box!.y);
    expect(asst1Box!.y).toBeLessThan(user2Box!.y);
    expect(user2Box!.y).toBeLessThan(asst2Box!.y);
    expect(asst2Box!.y).toBeLessThan(user3Box!.y);
    expect(user3Box!.y).toBeLessThan(asst3Box!.y);
    expect(asst3Box!.y).toBeLessThan(user4Box!.y);
  });

  test("turn-4 assistant slot shows '200' (not replays) once real answer arrives", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // SSE: replays followed by actual "200" still streaming (no turn_complete).
    await mockChatStream(page, CHAT_SESSION_ID, FOUR_TURN_CLI_REPLAY_SSE_WITH_INPROGRESS);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_FOUR_TURNS_IN_PROGRESS);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // All four user prompts visible.
    await expect(page.getByText("what is 100+100")).toBeVisible();

    // Prior answers appear exactly once each.
    await expect(page.getByText("4", { exact: true })).toHaveCount(1);
    await expect(page.getByText("16", { exact: true })).toHaveCount(1);
    await expect(page.getByText("32", { exact: true })).toHaveCount(1);

    // Real answer "200" appears exactly once for turn-4 — no replay duplicates.
    await expect(page.getByText("200", { exact: true })).toHaveCount(1);

    // "200" must appear AFTER "what is 100+100" (correct turn-4 position).
    const user4Box = await page.getByText("what is 100+100").boundingBox();
    const asst4Box = await page.getByText("200", { exact: true }).boundingBox();
    const asst3Box = await page.getByText("32", { exact: true }).boundingBox();

    expect(user4Box).not.toBeNull();
    expect(asst4Box).not.toBeNull();
    expect(asst3Box).not.toBeNull();

    // asst3("32") is before user4; asst4("200") is after user4.
    expect(asst3Box!.y).toBeLessThan(user4Box!.y);
    expect(user4Box!.y).toBeLessThan(asst4Box!.y);
  });
});
