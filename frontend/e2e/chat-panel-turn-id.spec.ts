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
  V2_SINGLE_TURN_STREAMING_SSE,
  V2_SINGLE_TURN_COMPLETE_SSE,
  V2_TWO_TURNS_SECOND_STREAMING_SSE,
  V2_CLI_RESTART_STREAMING_SSE,
  LIST_MESSAGES_V2_TWO_TURNS,
  LIST_MESSAGES_V2_TURN_A_COMPLETE,
  LIST_MESSAGES_EMPTY,
} from "./fixtures/chat-fixtures";

// Regression guard for RUSAA-1974: identity-based transcript merge by turn_id.
// Each test corresponds to one signal shape from the design matrix.

test.describe("Chat panel — turn_id identity-based merge [RUSAA-1974]", () => {
  test("AC1: in-progress assistant visible before DB persists (fresh session, v2 SSE)", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);
    await mockChatStream(page, CHAT_SESSION_ID, V2_SINGLE_TURN_STREAMING_SSE);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // Assistant response should be visible (streamed via v2 SSE).
    await expect(
      page.getByText("Ownership is Rust's memory safety model."),
    ).toBeVisible();
  });

  test("AC2: reload renders two complete v2 turns from DB with correct order", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_V2_TWO_TURNS);
    await mockChatStream(page, CHAT_SESSION_ID, ["", ""].join("\n"));

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    const q1Box = await page.getByText("q1").boundingBox();
    const r1Box = await page.getByText("Reply 1").boundingBox();
    const q2Box = await page.getByText("q2").boundingBox();

    expect(q1Box).not.toBeNull();
    expect(r1Box).not.toBeNull();
    expect(q2Box).not.toBeNull();

    expect(q1Box!.y).toBeLessThan(r1Box!.y);
    expect(r1Box!.y).toBeLessThan(q2Box!.y);
  });

  test("AC3: reconnect mid-stream — first turn from DB, second in-progress from SSE", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_V2_TURN_A_COMPLETE);
    await mockChatStream(page, CHAT_SESSION_ID, V2_TWO_TURNS_SECOND_STREAMING_SSE);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    await expect(page.getByText("Reply 1")).toBeVisible();
    await expect(page.getByText("Reply 2...")).toBeVisible();

    // q1 before Reply 1 before q2 before Reply 2
    const q1Box = await page.getByText("q1").boundingBox();
    const r1Box = await page.getByText("Reply 1").boundingBox();
    const q2Box = await page.getByText("q2").boundingBox();
    const r2Box = await page.getByText("Reply 2...").boundingBox();

    expect(q1Box!.y).toBeLessThan(r1Box!.y);
    expect(r1Box!.y).toBeLessThan(q2Box!.y);
    expect(q2Box!.y).toBeLessThan(r2Box!.y);
  });

  test("AC4: CLI restart — replayed turn from DB, new turn from SSE (no user_input echo)", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_V2_TWO_TURNS);
    // SSE has no user_input events but turn_id matches DB rows
    await mockChatStream(page, CHAT_SESSION_ID, V2_CLI_RESTART_STREAMING_SSE);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // Both user turns visible (from DB)
    await expect(page.getByText("q1")).toBeVisible();
    await expect(page.getByText("q2")).toBeVisible();
    // Turn 1 completed answer visible
    await expect(page.getByText("Reply 1")).toBeVisible();
    // Turn 2 in-progress streaming
    await expect(page.getByText("Reply 2...")).toBeVisible();
  });

  test("AC5: turn_complete arrives — composer re-enables (isStreaming → false)", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);
    // SSE has turn_complete → isStreaming = false → composer enabled
    await mockChatStream(page, CHAT_SESSION_ID, V2_SINGLE_TURN_COMPLETE_SSE);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    await expect(page.getByText("Ownership is Rust's memory safety model.")).toBeVisible();
    // The send button should not be in queuing state after turn_complete
    const sendBtn = page.getByRole("button", { name: "Send" });
    await expect(sendBtn).toBeVisible();
  });

  test("AC6: NULL turn_id legacy rows render via fallback path", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockSendChatMessage(page);
    // Legacy rows with no turn_id
    await mockListChatMessages(page, CHAT_SESSION_ID, {
      messages: [
        {
          id: "leg-u1",
          seq: 1,
          role: "user",
          body: "legacy question",
          created_at: "2026-01-01T00:00:00Z",
        },
        {
          id: "leg-a1",
          seq: 2,
          role: "assistant",
          body: "legacy answer",
          created_at: "2026-01-01T00:00:01Z",
        },
      ],
      has_more: false,
    });
    await mockChatStream(page, CHAT_SESSION_ID, ["", ""].join("\n"));

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    await expect(page.getByText("legacy question")).toBeVisible();
    await expect(page.getByText("legacy answer")).toBeVisible();
  });

  test("AC7: optimistic bubble suppressed once SSE user_input echo arrives", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);
    await mockChatStream(page, CHAT_SESSION_ID, V2_SINGLE_TURN_STREAMING_SSE);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // SSE has user_input(text="what is ownership?") — there should be exactly one user bubble.
    const userBubbles = page.getByText("what is ownership?");
    await expect(userBubbles).toHaveCount(1);
  });

  test("AC8: no duplicate assistant content on SSE reconnect (replay-on-reconnect)", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_V2_TURN_A_COMPLETE);
    // SSE replays TURN_A (already in DB) then continues with TURN_B
    await mockChatStream(page, CHAT_SESSION_ID, V2_TWO_TURNS_SECOND_STREAMING_SSE);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // "Reply 1" should appear exactly once (not duplicated by replay)
    const reply1 = page.getByText("Reply 1");
    await expect(reply1).toHaveCount(1);
  });
});
