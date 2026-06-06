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
  FOUR_TURN_ASSISTANT_ONLY_SSE,
  LIST_MESSAGES_FOUR_TURNS,
} from "./fixtures/chat-mock-api";

// Regression guard for RUSAA-1934: when SSE delivers assistant chunks without
// user_input echoes (firstLiveUser=null), prior-turn assistant responses must
// not be duplicated after the latest turn's response.
//
// Symptom (pre-fix): transcript showed user1…asst3, user4, asst1', asst2', asst3', asst4
// because the !firstLiveUser branch appended all liveItems after the full history.

test.describe("Chat panel — 4-turn dedup when SSE has no user_input echoes (RUSAA-1934)", () => {
  test("prior-turn assistant responses are not duplicated below the latest turn", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // SSE: 4 assistant turns, turn_complete after each of 1-3, turn-4 in progress.
    // No user_input events — exercises the firstLiveUser=null dedup path.
    await mockChatStream(page, CHAT_SESSION_ID, FOUR_TURN_ASSISTANT_ONLY_SSE);
    await mockSendChatMessage(page);
    // History: 3 complete turns + user-4 in-flight (no asst-4 yet).
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_FOUR_TURNS);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // All 4 user and 4 assistant items must be visible.
    await expect(page.getByText("what is 2+@")).toBeVisible();
    await expect(page.getByText("2+2 equals 4.")).toBeVisible();
    await expect(page.getByText("@ is 5")).toBeVisible();
    await expect(page.getByText("2 plus 5 is 7.")).toBeVisible();
    await expect(page.getByText("what is @+7")).toBeVisible();
    await expect(page.getByText("5 plus 7 is 12.")).toBeVisible();
    await expect(page.getByText("what is @+12")).toBeVisible();
    await expect(page.getByText("5 plus 12 is 17.")).toBeVisible();

    // No duplicates: each assistant response must appear exactly once.
    await expect(page.getByText("2+2 equals 4.")).toHaveCount(1);
    await expect(page.getByText("2 plus 5 is 7.")).toHaveCount(1);
    await expect(page.getByText("5 plus 7 is 12.")).toHaveCount(1);
    await expect(page.getByText("5 plus 12 is 17.")).toHaveCount(1);

    // Strict top-to-bottom ordering via bounding box comparison.
    const user1Box = await page.getByText("what is 2+@").boundingBox();
    const asst1Box = await page.getByText("2+2 equals 4.").boundingBox();
    const user2Box = await page.getByText("@ is 5").boundingBox();
    const asst2Box = await page.getByText("2 plus 5 is 7.").boundingBox();
    const user3Box = await page.getByText("what is @+7").boundingBox();
    const asst3Box = await page.getByText("5 plus 7 is 12.").boundingBox();
    const user4Box = await page.getByText("what is @+12").boundingBox();
    const asst4Box = await page.getByText("5 plus 12 is 17.").boundingBox();

    expect(user1Box).not.toBeNull();
    expect(asst1Box).not.toBeNull();
    expect(user2Box).not.toBeNull();
    expect(asst2Box).not.toBeNull();
    expect(user3Box).not.toBeNull();
    expect(asst3Box).not.toBeNull();
    expect(user4Box).not.toBeNull();
    expect(asst4Box).not.toBeNull();

    expect(user1Box!.y).toBeLessThan(asst1Box!.y);
    expect(asst1Box!.y).toBeLessThan(user2Box!.y);
    expect(user2Box!.y).toBeLessThan(asst2Box!.y);
    expect(asst2Box!.y).toBeLessThan(user3Box!.y);
    expect(user3Box!.y).toBeLessThan(asst3Box!.y);
    expect(asst3Box!.y).toBeLessThan(user4Box!.y);
    // asst4 is in-progress and must appear after user4, not before it.
    expect(user4Box!.y).toBeLessThan(asst4Box!.y);
  });
});
