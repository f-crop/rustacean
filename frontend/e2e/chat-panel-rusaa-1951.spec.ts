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
  TOOL_USE_WITH_INTERMEDIATE_TURN_COMPLETE_SSE,
  LIST_MESSAGES_SPLIT_TOOL_TURN,
  LIST_MESSAGES_WITH_TOOL_USE,
} from "./fixtures/chat-mock-api";

// Regression guard for RUSAA-1951 (R28): tool_use/tool_result blocks not rendered.
//
// Root cause: buildTranscript flushed pendingAssistant on every turn_complete,
// including the intermediate turn_complete(stop_reason="tool_use") that claude-code
// emits when the model pauses for tool execution. This split tool_use into one
// AssistantTranscriptItem and tool_result into another, so findToolResult could not
// match them and ToolCallBlock always rendered as "Running…" with no result.
//
// Fix 1 (live render): buildTranscript skips flush when stop_reason==="tool_use".
// Fix 2 (reload): buildTranscriptFromHistory merges consecutive assistant rows where
//   the first ends with tool_use (split-batch ingest artefact).

test.describe("Chat panel — tool_use/tool_result rendering [RUSAA-1951]", () => {
  test("AC1 live: tool_use block visible with result when turn_complete(tool_use) is in SSE stream", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockChatStream(
      page,
      CHAT_SESSION_ID,
      TOOL_USE_WITH_INTERMEDIATE_TURN_COMPLETE_SSE,
    );
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // User message is visible
    await expect(page.getByText("Search for rust async patterns")).toBeVisible();

    // ToolCallBlock must be visible with the tool name
    const toolBlock = page.getByTestId("tool-call-block");
    await expect(toolBlock).toBeVisible();

    // The tool block should show "Done" (result received), not "Running…"
    await expect(toolBlock).toContainText("Done");
    await expect(toolBlock).toContainText("mcp__rust_brain__recall");

    // The final text answer is also visible
    await expect(page.getByText("Here are the async patterns I found.")).toBeVisible();
  });

  test("AC1 live: tool_use block shows Running state while tool_result not yet received", async ({
    page,
  }) => {
    // Only tool_use and the intermediate turn_complete — no tool_result yet (tool still running)
    const partialSse = [
      "event: session.event",
      `data: ${JSON.stringify({
        session_id: CHAT_SESSION_ID,
        event_type: "user_input",
        sequence: 1,
        payload: { type: "user_input", text: "Search for rust async patterns" },
      })}`,
      "",
      "event: session.event",
      `data: ${JSON.stringify({
        session_id: CHAT_SESSION_ID,
        event_type: "tool_use",
        sequence: 2,
        payload: {
          type: "tool_use",
          id: "tu-partial",
          name: "mcp__rust_brain__recall",
          input: { tags: ["rust"] },
        },
      })}`,
      "",
      "event: session.event",
      `data: ${JSON.stringify({
        session_id: CHAT_SESSION_ID,
        event_type: "turn_complete",
        sequence: 3,
        payload: { type: "turn_complete", stop_reason: "tool_use" },
      })}`,
      "",
      "",
    ].join("\n");

    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockChatStream(page, CHAT_SESSION_ID, partialSse);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // ToolCallBlock visible, still Running (no result yet)
    const toolBlock = page.getByTestId("tool-call-block");
    await expect(toolBlock).toBeVisible();
    await expect(toolBlock).toContainText("Running");
    await expect(toolBlock).toContainText("mcp__rust_brain__recall");
  });

  test("AC2 reload: single-row history renders ToolCallBlock with result (ideal case)", async ({
    page,
  }) => {
    // LIST_MESSAGES_WITH_TOOL_USE has tool_use + tool_result + text in one DB row.
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // No live SSE events — pure reload from history
    await mockChatStream(page, CHAT_SESSION_ID, ["", ""].join("\n"));
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_WITH_TOOL_USE);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    await expect(page.getByText("Search for recent Rust news")).toBeVisible();

    const toolBlock = page.getByTestId("tool-call-block");
    await expect(toolBlock).toBeVisible();
    await expect(toolBlock).toContainText("Done");
    await expect(toolBlock).toContainText("mcp__rust_brain__search_demo");

    await expect(page.getByText("Here are the recent Rust news results.")).toBeVisible();
  });

  test("AC2 reload: split-batch history (tool_use + tool_result in separate rows) renders ToolCallBlock with result", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockChatStream(page, CHAT_SESSION_ID, ["", ""].join("\n"));
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_SPLIT_TOOL_TURN);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    await expect(page.getByText("Search for rust async patterns")).toBeVisible();

    const toolBlock = page.getByTestId("tool-call-block");
    await expect(toolBlock).toBeVisible();
    // Must show "Done" with result — not "Running…" (which would indicate the merge failed)
    await expect(toolBlock).toContainText("Done");
    await expect(toolBlock).toContainText("mcp__rust_brain__recall");

    await expect(page.getByText("Here are the async patterns I found.")).toBeVisible();
  });

  test("AC3 regression: R27 ordering still correct — tool-using turn does not bleed into adjacent turns", async ({
    page,
  }) => {
    // Two-turn session: turn-1 uses a tool, turn-2 is plain text.
    // Verify turn-1 tool block and turn-2 text appear in correct slots.
    const twoTurnWithToolSse = [
      "event: session.event",
      `data: ${JSON.stringify({
        session_id: CHAT_SESSION_ID,
        event_type: "user_input",
        sequence: 1,
        payload: { type: "user_input", text: "find files" },
      })}`,
      "",
      "event: session.event",
      `data: ${JSON.stringify({
        session_id: CHAT_SESSION_ID,
        event_type: "tool_use",
        sequence: 2,
        payload: { type: "tool_use", id: "t1", name: "bash", input: { cmd: "ls" } },
      })}`,
      "",
      "event: session.event",
      `data: ${JSON.stringify({
        session_id: CHAT_SESSION_ID,
        event_type: "turn_complete",
        sequence: 3,
        payload: { type: "turn_complete", stop_reason: "tool_use" },
      })}`,
      "",
      "event: session.event",
      `data: ${JSON.stringify({
        session_id: CHAT_SESSION_ID,
        event_type: "tool_result",
        sequence: 4,
        payload: {
          type: "tool_result",
          tool_use_id: "t1",
          content: "file1.txt",
          is_error: false,
        },
      })}`,
      "",
      "event: session.event",
      `data: ${JSON.stringify({
        session_id: CHAT_SESSION_ID,
        event_type: "text",
        sequence: 5,
        payload: { type: "text", text: "Found file1.txt" },
      })}`,
      "",
      "event: session.event",
      `data: ${JSON.stringify({
        session_id: CHAT_SESSION_ID,
        event_type: "turn_complete",
        sequence: 6,
        payload: { type: "turn_complete", stop_reason: "end_turn" },
      })}`,
      "",
      "event: session.event",
      `data: ${JSON.stringify({
        session_id: CHAT_SESSION_ID,
        event_type: "user_input",
        sequence: 7,
        payload: { type: "user_input", text: "what is rust?" },
      })}`,
      "",
      "event: session.event",
      `data: ${JSON.stringify({
        session_id: CHAT_SESSION_ID,
        event_type: "text",
        sequence: 8,
        payload: { type: "text", text: "Rust is a systems language." },
      })}`,
      "",
      "event: session.event",
      `data: ${JSON.stringify({
        session_id: CHAT_SESSION_ID,
        event_type: "turn_complete",
        sequence: 9,
        payload: { type: "turn_complete", stop_reason: "end_turn" },
      })}`,
      "",
      "",
    ].join("\n");

    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockChatStream(page, CHAT_SESSION_ID, twoTurnWithToolSse);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // Tool block in turn-1
    await expect(page.getByTestId("tool-call-block")).toBeVisible();
    await expect(page.getByText("Found file1.txt")).toBeVisible();

    // Turn-2 answer is visible
    await expect(page.getByText("Rust is a systems language.")).toBeVisible();

    // Ordering: "find files" < tool block < "Found file1.txt" < "what is rust?" < "Rust is a systems language."
    const user1Box = await page.getByText("find files").boundingBox();
    const toolBox = await page.getByTestId("tool-call-block").boundingBox();
    const answer1Box = await page.getByText("Found file1.txt").boundingBox();
    const user2Box = await page.getByText("what is rust?").boundingBox();
    const answer2Box = await page.getByText("Rust is a systems language.").boundingBox();

    expect(user1Box).not.toBeNull();
    expect(toolBox).not.toBeNull();
    expect(answer1Box).not.toBeNull();
    expect(user2Box).not.toBeNull();
    expect(answer2Box).not.toBeNull();

    expect(user1Box!.y).toBeLessThan(toolBox!.y);
    expect(toolBox!.y).toBeLessThan(answer1Box!.y);
    expect(answer1Box!.y).toBeLessThan(user2Box!.y);
    expect(user2Box!.y).toBeLessThan(answer2Box!.y);
  });
});
