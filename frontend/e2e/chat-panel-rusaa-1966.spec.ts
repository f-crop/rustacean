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
  LIST_MESSAGES_R31_THREE_TURNS_SPLIT_TURN2,
  R31_CLI_RESTART_SPLIT_TURN2_ALL_COMPLETE_SSE,
  LIST_MESSAGES_R31_LIVE_SPLIT_TURN2,
  R31_LIVE_SESSION_SPLIT_TURN2_ALL_COMPLETE_SSE,
} from "./fixtures/chat-mock-api-r31";

// Regression guard for RUSAA-1966 (R31): prior turn text output disappears when a new
// turn completes, in sessions where a turn has text output followed by tool calls.
//
// Root cause: the agent-runner persists the initial text response as a separate DB row
// before the tool call starts, creating two consecutive assistant rows for the same turn:
//   row A: seq=5, body=[{type:"text", text:"Searching…"}]
//   row B: seq=7, body=[{type:"tool_use", …}, {type:"tool_result", …}]
//
// buildTranscriptFromHistory split-batch merge only checked if the PREVIOUS row ended
// with tool_use — it did not merge when the NEXT row starts with tool_use. So history
// produced 2 AssistantTranscriptItems for turn 2 instead of 1.
//
// When turn 3 completed (hasLiveInProgress → false), dedupeAssistantsPerSegment ran on
// user-2's segment and saw completedCount=2 items. replayCount = min(histSeqs.size, 2)
// was ≥ 1, causing the text item (asst2_text) to be dropped.
//
// Fix (transcript.ts):
//   1. buildTranscriptFromHistory: extend split-batch merge condition to also fire when
//      contentBlocks[0].type === "tool_use" (next row starts with tool_use).
//   2. buildTranscript: post-process with mergeAdjacentToolUseAssistants to merge
//      consecutive live assistant items where the second starts with tool_use (same
//      split pattern can occur in the SSE stream when end_turn fires before tool_use).

test.describe("Chat panel — prior turn text preserved when new turn completes [RUSAA-1966]", () => {
  test("AC1: !firstLiveUser — turn-2 text remains visible after all turns complete (CLI restart, split DB rows)", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockSendChatMessage(page);
    // History: 3 turns, turn-2 split across two DB rows (text-only + tool_use).
    await mockListChatMessages(
      page,
      CHAT_SESSION_ID,
      LIST_MESSAGES_R31_THREE_TURNS_SPLIT_TURN2,
    );
    // SSE: CLI restart (no user_input); replays all three turns with the RUSAA-1966 split
    // pattern (turn_complete(end_turn) between text and tool_use for turn 2).
    // All turns complete — this triggers dedupeAssistantsPerSegment on turn-2's segment.
    await mockChatStream(
      page,
      CHAT_SESSION_ID,
      R31_CLI_RESTART_SPLIT_TURN2_ALL_COMPLETE_SSE,
    );

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // All three user bubbles must be visible.
    await expect(page.getByText("what is rust brain?", { exact: true })).toBeVisible();
    await expect(page.getByText("search for rust examples", { exact: true })).toBeVisible();
    await expect(page.getByText("show details", { exact: true })).toBeVisible();

    // Turn-2 text (the regression victim: this text disappeared without the fix).
    await expect(page.getByText("Searching for rust examples...")).toBeVisible();

    // Turn-2 tool block must be visible (the tool call that follows the text).
    const toolBlocks = page.getByTestId("tool-call-block");
    await expect(toolBlocks).toHaveCount(1);
    await expect(toolBlocks.first()).toContainText("mcp__rust_brain__search_items");
    await expect(toolBlocks.first()).toContainText("Done");

    // Turn-3 text must be visible.
    await expect(page.getByText("Here are the details for the first result.")).toBeVisible();

    // Strict ordering:
    //   user-2 < turn-2 text < tool block < user-3 < turn-3 text
    const user2Box = await page
      .getByText("search for rust examples", { exact: true })
      .boundingBox();
    const turn2TextBox = await page
      .getByText("Searching for rust examples...")
      .boundingBox();
    const toolBox = await toolBlocks.first().boundingBox();
    const user3Box = await page.getByText("show details", { exact: true }).boundingBox();
    const turn3TextBox = await page
      .getByText("Here are the details for the first result.")
      .boundingBox();

    expect(user2Box).not.toBeNull();
    expect(turn2TextBox).not.toBeNull();
    expect(toolBox).not.toBeNull();
    expect(user3Box).not.toBeNull();
    expect(turn3TextBox).not.toBeNull();

    expect(user2Box!.y).toBeLessThan(turn2TextBox!.y);
    expect(turn2TextBox!.y).toBeLessThan(toolBox!.y);
    expect(toolBox!.y).toBeLessThan(user3Box!.y);
    expect(user3Box!.y).toBeLessThan(turn3TextBox!.y);
  });

  test("AC2: !firstLiveUser — pure reload from history still shows turn-2 text + tool block", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockSendChatMessage(page);
    await mockListChatMessages(
      page,
      CHAT_SESSION_ID,
      LIST_MESSAGES_R31_THREE_TURNS_SPLIT_TURN2,
    );
    // No live SSE events — pure history render; exercises buildTranscriptFromHistory fix.
    await mockChatStream(page, CHAT_SESSION_ID, ["", ""].join("\n"));

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    await expect(page.getByText("Searching for rust examples...")).toBeVisible();

    const toolBlocks = page.getByTestId("tool-call-block");
    await expect(toolBlocks).toHaveCount(1);
    await expect(toolBlocks.first()).toContainText("mcp__rust_brain__search_items");
    await expect(toolBlocks.first()).toContainText("Done");

    await expect(page.getByText("Here are the details for the first result.")).toBeVisible();

    // Text must appear before tool block in the same assistant bubble.
    const turn2TextBox = await page
      .getByText("Searching for rust examples...")
      .boundingBox();
    const toolBox = await toolBlocks.first().boundingBox();
    expect(turn2TextBox).not.toBeNull();
    expect(toolBox).not.toBeNull();
    expect(turn2TextBox!.y).toBeLessThan(toolBox!.y);
  });

  test("AC3: firstLiveUser — live session with end_turn between text and tool_use preserves text after turn-3 completes", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockSendChatMessage(page);
    // History: same split structure (used for histAssistantSeqs and dedup).
    await mockListChatMessages(
      page,
      CHAT_SESSION_ID,
      LIST_MESSAGES_R31_LIVE_SPLIT_TURN2,
    );
    // SSE: full live session (has user_input events).  Turn 2 has text(end_turn)+tool_use
    // in separate events.  Turn 3 completes at the end — triggers dedupeAssistantsPerSegment.
    await mockChatStream(
      page,
      CHAT_SESSION_ID,
      R31_LIVE_SESSION_SPLIT_TURN2_ALL_COMPLETE_SSE,
    );

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // Turn-2 text must be present (regression: buildTranscript post-processing fix).
    await expect(page.getByText("Searching for rust examples...")).toBeVisible();

    const toolBlocks = page.getByTestId("tool-call-block");
    await expect(toolBlocks).toHaveCount(1);
    await expect(toolBlocks.first()).toContainText("mcp__rust_brain__search_items");

    await expect(page.getByText("Here are the details for the first result.")).toBeVisible();

    // turn-2 text must appear before tool block.
    const turn2TextBox = await page
      .getByText("Searching for rust examples...")
      .boundingBox();
    const toolBox = await toolBlocks.first().boundingBox();
    expect(turn2TextBox).not.toBeNull();
    expect(toolBox).not.toBeNull();
    expect(turn2TextBox!.y).toBeLessThan(toolBox!.y);
  });
});
