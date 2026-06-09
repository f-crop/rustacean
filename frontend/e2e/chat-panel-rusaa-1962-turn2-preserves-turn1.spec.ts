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
  LIST_MESSAGES_R29_TOOL_USE_TURN1_TURN2_USER,
  TOOL_USE_REPLAY_THEN_TURN2_STREAMING_SSE,
  LIST_MESSAGES_R29_THREE_TURNS_TURN3_USER,
  TOOL_USE_TWO_REPLAYS_THEN_TURN3_STREAMING_SSE,
} from "./fixtures/chat-mock-api-cli-restart-r29";

// Regression guard for RUSAA-1962 (R29 fixup): sending turn-2 wipes turn-1
// transcript when turn-1 used tool calls and the CLI restarts without emitting
// user_input events.
//
// Root cause (PR #734 regression): turn_complete(stop_reason="tool_use") in the
// CLI replay is correctly NOT flushed (PR #734 fix for live rendering). This means
// the replay now produces ONE flushed assistant item (instead of two as before).
// In the !firstLiveUser path, dedupeAssistantsPerSegment was called with
// [...histItems(user-1, assistant-1), extraLive(assistant-2(inProgress))]. The
// dedup placed assistant-2(inProgress) in user-1's segment alongside assistant-1.
// The "has inProgress → keep only last inProgress" rule then dropped assistant-1,
// wiping all of turn-1 from the transcript.
//
// Fix: when hasLiveInProgress is true in the !firstLiveUser path, append extraLive
// directly after histItems without running them through dedupeAssistantsPerSegment
// together. The dedup is only needed to handle completed replay slippage; it is
// unnecessary (and harmful) when all completed replays were already dropped by the
// extraLive filter.

test.describe("Chat panel — turn-2 preserves turn-1 transcript [RUSAA-1962]", () => {
  test("AC1: turn-1 tool-use content remains visible when turn-2 starts streaming (CLI restart, no user_input)", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockSendChatMessage(page);
    // History: turn-1 complete (user-1 + assistant-1 with tool use), turn-2 user stored
    await mockListChatMessages(
      page,
      CHAT_SESSION_ID,
      LIST_MESSAGES_R29_TOOL_USE_TURN1_TURN2_USER,
    );
    // SSE: no user_input; CLI replays turn-1 with intermediate turn_complete(tool_use),
    // then starts streaming turn-2's new tool call.
    await mockChatStream(page, CHAT_SESSION_ID, TOOL_USE_REPLAY_THEN_TURN2_STREAMING_SSE);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // Turn-1 user bubble must be visible.
    await expect(page.getByText("in rust brain", { exact: true })).toBeVisible();

    // Turn-1 assistant tool block must be visible (this is the regression: it was dropped).
    const toolBlocks = page.getByTestId("tool-call-block");
    await expect(toolBlocks.first()).toBeVisible();
    // The turn-1 tool block shows "Done" (replay replay has result).
    await expect(toolBlocks.first()).toContainText("mcp_rust_brain_search_items");

    // Turn-1 assistant text must be visible.
    await expect(page.getByText("Here is what I found in rust brain.")).toBeVisible();

    // Turn-2 user bubble must be visible.
    await expect(page.getByText("show me more")).toBeVisible();

    // Turn-2 tool block must be visible (streaming inProgress).
    await expect(toolBlocks.last()).toContainText("mcp_rust_brain_get_item");

    // Ordering: turn-1 user < turn-1 tool block < turn-1 text < turn-2 user < turn-2 tool block
    const turn1UserBox = await page.getByText("in rust brain", { exact: true }).boundingBox();
    const turn1ToolBox = await toolBlocks.first().boundingBox();
    const turn1TextBox = await page
      .getByText("Here is what I found in rust brain.")
      .boundingBox();
    const turn2UserBox = await page.getByText("show me more").boundingBox();
    const turn2ToolBox = await toolBlocks.last().boundingBox();

    expect(turn1UserBox).not.toBeNull();
    expect(turn1ToolBox).not.toBeNull();
    expect(turn1TextBox).not.toBeNull();
    expect(turn2UserBox).not.toBeNull();
    expect(turn2ToolBox).not.toBeNull();

    expect(turn1UserBox!.y).toBeLessThan(turn1ToolBox!.y);
    expect(turn1ToolBox!.y).toBeLessThan(turn2UserBox!.y);
    expect(turn2UserBox!.y).toBeLessThan(turn2ToolBox!.y);
  });

  test("AC2: reload after R29 fix still shows turn-1 tool block correctly (R28 guard)", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockSendChatMessage(page);
    await mockListChatMessages(
      page,
      CHAT_SESSION_ID,
      LIST_MESSAGES_R29_TOOL_USE_TURN1_TURN2_USER,
    );
    // No live SSE events — pure reload from history.
    await mockChatStream(page, CHAT_SESSION_ID, ["", ""].join("\n"));

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    await expect(page.getByText("in rust brain", { exact: true })).toBeVisible();
    const toolBlock = page.getByTestId("tool-call-block");
    await expect(toolBlock).toBeVisible();
    await expect(toolBlock).toContainText("Done");
    await expect(toolBlock).toContainText("mcp_rust_brain_search_items");
    await expect(page.getByText("Here is what I found in rust brain.")).toBeVisible();
  });

  test("AC3: three-turn extension preserves turns 1+2 when turn-3 starts streaming", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockSendChatMessage(page);
    await mockListChatMessages(
      page,
      CHAT_SESSION_ID,
      LIST_MESSAGES_R29_THREE_TURNS_TURN3_USER,
    );
    await mockChatStream(
      page,
      CHAT_SESSION_ID,
      TOOL_USE_TWO_REPLAYS_THEN_TURN3_STREAMING_SSE,
    );

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // All three user bubbles visible.
    await expect(page.getByText("in rust brain", { exact: true })).toBeVisible();
    await expect(page.getByText("show me more")).toBeVisible();
    await expect(page.getByText("what else?")).toBeVisible();

    // All three tool blocks visible.
    const toolBlocks = page.getByTestId("tool-call-block");
    await expect(toolBlocks).toHaveCount(3);

    // Turn-1 and turn-2 tool blocks show "Done".
    await expect(toolBlocks.nth(0)).toContainText("mcp_rust_brain_search_items");
    await expect(toolBlocks.nth(1)).toContainText("mcp_rust_brain_get_item");
    // Turn-3 is still streaming.
    await expect(toolBlocks.nth(2)).toContainText("mcp_rust_brain_list");

    // Strict ordering: user-1 < tool-1 < user-2 < tool-2 < user-3 < tool-3
    const u1Box = await page.getByText("in rust brain", { exact: true }).boundingBox();
    const a1Box = await toolBlocks.nth(0).boundingBox();
    const u2Box = await page.getByText("show me more").boundingBox();
    const a2Box = await toolBlocks.nth(1).boundingBox();
    const u3Box = await page.getByText("what else?").boundingBox();
    const a3Box = await toolBlocks.nth(2).boundingBox();

    expect(u1Box).not.toBeNull();
    expect(a1Box).not.toBeNull();
    expect(u2Box).not.toBeNull();
    expect(a2Box).not.toBeNull();
    expect(u3Box).not.toBeNull();
    expect(a3Box).not.toBeNull();

    expect(u1Box!.y).toBeLessThan(a1Box!.y);
    expect(a1Box!.y).toBeLessThan(u2Box!.y);
    expect(u2Box!.y).toBeLessThan(a2Box!.y);
    expect(a2Box!.y).toBeLessThan(u3Box!.y);
    expect(u3Box!.y).toBeLessThan(a3Box!.y);
  });
});
