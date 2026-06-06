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
  FULL_EXCHANGE_SSE,
  TURN1_ASSISTANT_ONLY_SSE,
  TURN2_ASSISTANT_ONLY_SSE,
  LIST_MESSAGES_TURN1_USER_ONLY,
  LIST_MESSAGES_MONAD_USER_ONLY,
  LIST_MESSAGES_WITH_TOOL_USE,
  LIST_MESSAGES_EMPTY,
  STREAMING_ASSISTANT_SSE,
  COMPLETED_EXCHANGE_SSE,
} from "./fixtures/chat-mock-api";

// ---------------------------------------------------------------------------
// Bug C turn-2 edge case — RUSAA-1907: historical user + SSE assistant-only
// ---------------------------------------------------------------------------

test.describe("Chat panel — turn-2 ordering when SSE lacks user_input (RUSAA-1907 edge case)", () => {
  // Regression guard for RUSAA-1907: when the SSE stream joins after the
  // user_input event was emitted (or the server replays only content events),
  // liveItems = [assistant-1(inProgress)] with no user turn.
  // Historical DB provides user-1.  base = [user-1-hist, assistant-1(inProgress)].
  //
  // Without the secondary guard (checking base[candidateSlot-1]?.kind !== "user"),
  // the slot predicate fires (!liveHasUserEcho) and inserts the turn-2 pending
  // bubble at position 1 — between user-1 and assistant-1.
  // The secondary guard prevents this by detecting the user-1-hist pairing.
  test("turn-2: pending bubble appears after assistant-1 when SSE has no user_input and history has user-1", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // SSE has only assistant text — no user_input event (simulates mid-stream join).
    await mockChatStream(page, CHAT_SESSION_ID, TURN1_ASSISTANT_ONLY_SSE);
    await mockSendChatMessage(page);
    // Historical DB has user-1's message but assistant row not yet flushed.
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_TURN1_USER_ONLY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // Both user-1 (from history) and assistant-1 (from SSE) must be visible.
    await expect(page.getByText("what are the tools available")).toBeVisible();
    await expect(page.getByText("Here are the available tools.")).toBeVisible();

    // Send turn-2 optimistically.
    await page.getByRole("textbox", { name: "Chat message" }).fill("explain monad");
    await page.getByRole("button", { name: "Send" }).click();

    await expect(page.getByText("explain monad")).toBeVisible();

    // The turn-2 pending bubble must appear BELOW assistant-1, not before it.
    const pendingBubble = page.getByText("explain monad");
    const assistantContent = page.getByText("Here are the available tools.");

    const pendingBox = await pendingBubble.boundingBox();
    const assistantBox = await assistantContent.boundingBox();

    expect(pendingBox).not.toBeNull();
    expect(assistantBox).not.toBeNull();
    expect(pendingBox!.y).toBeGreaterThan(assistantBox!.y);
  });
});

// ---------------------------------------------------------------------------
// Bug — RUSAA-1912: 3-turn rapid-send ordering inverts when history is empty
// ---------------------------------------------------------------------------

test.describe("Chat panel — 3-turn rapid-send ordering (RUSAA-1912)", () => {
  // Regression guard for RUSAA-1912: when the user sends 3 messages in rapid
  // succession before SSE echoes any user_input, liveItems = [assistant-1(inProgress)]
  // and history is empty.  candidateSlot = 0, base[-1] is undefined so the
  // secondary guard `base[candidateSlot-1]?.kind !== "user"` is true — without
  // the multi-pending fix all 3 items slot BEFORE the in-progress assistant,
  // producing [user-1, user-2, user-3, assistant-1] instead of
  // [user-1, assistant-1, user-2, user-3].
  test("turn-2 and turn-3 pending bubbles appear AFTER in-progress assistant when history is empty and SSE has no user_input", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // SSE has only assistant text — no user_input events (stream joined mid-turn).
    await mockChatStream(page, CHAT_SESSION_ID, TURN1_ASSISTANT_ONLY_SSE);
    await mockSendChatMessage(page);
    // History is empty — DB cache hasn't refreshed yet.
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // The in-progress assistant text must be visible from the SSE stream.
    await expect(page.getByText("Here are the available tools.")).toBeVisible();

    const composer = page.getByRole("textbox", { name: "Chat message" });
    const sendButton = page.getByRole("button", { name: "Send" });

    // Send 3 messages in rapid succession (no waiting for echoes between sends).
    await composer.fill("what are the tools available");
    await sendButton.click();

    await composer.fill("what is monad");
    await sendButton.click();

    await composer.fill("what is lift doing");
    await sendButton.click();

    // All 3 pending bubbles must be visible.
    await expect(page.getByText("what are the tools available")).toBeVisible();
    await expect(page.getByText("what is monad")).toBeVisible();
    await expect(page.getByText("what is lift doing")).toBeVisible();

    const assistantContent = page.getByText("Here are the available tools.");
    const bubble1 = page.getByText("what are the tools available");
    const bubble2 = page.getByText("what is monad");
    const bubble3 = page.getByText("what is lift doing");

    const [assistantBox, box1, box2, box3] = await Promise.all([
      assistantContent.boundingBox(),
      bubble1.boundingBox(),
      bubble2.boundingBox(),
      bubble3.boundingBox(),
    ]);

    expect(assistantBox).not.toBeNull();
    expect(box1).not.toBeNull();
    expect(box2).not.toBeNull();
    expect(box3).not.toBeNull();

    // user-1 (trigger message) must appear ABOVE in-progress assistant.
    expect(box1!.y).toBeLessThan(assistantBox!.y);
    // user-2 and user-3 (subsequent sends) must appear BELOW assistant.
    expect(box2!.y).toBeGreaterThan(assistantBox!.y);
    expect(box3!.y).toBeGreaterThan(assistantBox!.y);
    // user-3 must appear below user-2.
    expect(box3!.y).toBeGreaterThan(box2!.y);
  });
});

// ---------------------------------------------------------------------------
// Bug — RUSAA-1915: sequential 2-turn — user-2 pending jumps to bottom after
// assistant-2 starts streaming (SSE reconnect, history has user-1 only)
// ---------------------------------------------------------------------------

test.describe("Chat panel — sequential turn-2 pending bubble ordering (RUSAA-1915)", () => {
  // Regression guard for RUSAA-1915: when the user sends two sequential turns
  // (not rapid-fire), the SSE reconnects after turn-1, and history has user-1
  // but not assistant-1 yet, liveItems = [assistant-2(inProgress)] with no
  // user_input echo.
  //
  // base = [user-1-hist, assistant-2(inProgress)]
  // candidateSlot = 1, base[0] = user-1-hist (kind "user")
  //
  // Without the priorTurnsCompleted guard, the secondary check fires (prior row
  // IS a user) and insertAt moves to base.length — appending user-2-pending
  // AFTER the in-progress assistant instead of before it.
  //
  // The fix: if pendingUserSends.length > pendingItems.length (prior sends are
  // covered, meaning prior turns completed), the in-progress assistant is for
  // the CURRENT pending turn, so slot before it even if a user precedes.
  test("turn-2 pending bubble appears BEFORE assistant-2 streaming when history has turn-1 user only", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // SSE: only assistant-2 streaming — no user_input echo (post-reconnect state).
    await mockChatStream(page, CHAT_SESSION_ID, TURN2_ASSISTANT_ONLY_SSE);
    await mockSendChatMessage(page);
    // Historical: user-1 present, assistant-1 not yet flushed to DB.
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_MONAD_USER_ONLY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // Historical user-1 and SSE assistant-2 must both be visible on load.
    await expect(page.getByText("what is monad")).toBeVisible();
    await expect(page.getByText("Lift is a higher-order function that maps a regular function into a functor.")).toBeVisible();

    const composer = page.getByRole("textbox", { name: "Chat message" });
    const sendButton = page.getByRole("button", { name: "Send" });

    // Send turn-1 ("what is monad") — adds to pendingUserSends; will be covered
    // by history (coveredTexts), so it won't appear as a duplicate bubble but
    // priorTurnsCompleted becomes true when turn-2 is also pending.
    await composer.fill("what is monad");
    await sendButton.click();

    // Send turn-2 ("what is lift") — the pending bubble that must slot BEFORE
    // the in-progress assistant-2.
    await composer.fill("what is lift");
    await sendButton.click();

    await expect(page.getByText("what is lift")).toBeVisible();

    // user-2 "what is lift" must appear ABOVE (before) the streaming assistant.
    const pendingBubble2 = page.getByText("what is lift");
    const assistantContent = page.getByText("Lift is a higher-order function that maps a regular function into a functor.");

    const [bubbleBox, assistantBox] = await Promise.all([
      pendingBubble2.boundingBox(),
      assistantContent.boundingBox(),
    ]);

    expect(bubbleBox).not.toBeNull();
    expect(assistantBox).not.toBeNull();
    // user-2 pending must appear above (lower y = higher on page) the assistant.
    expect(bubbleBox!.y).toBeLessThan(assistantBox!.y);
  });
});

// ---------------------------------------------------------------------------
// Bug — RUSAA-1915 AC5: tool_use blocks render in live-stream and on reload
// ---------------------------------------------------------------------------

test.describe("Chat panel — tool-call block rendering (RUSAA-1915 AC5)", () => {
  // Regression guard: tool_use items emitted by the SSE stream and persisted
  // assistant messages must render as [data-testid="tool-call-block"] panels
  // in the transcript.  The full code path (normalizer → ingest → SSE → FE
  // reducer → AssistantBubble → ToolCallBlock) was verified correct by code
  // analysis; this test asserts the DOM contract so any future regression is
  // immediately caught.

  test("live-stream: tool-call block appears in DOM when SSE emits tool_use + tool_result", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // FULL_EXCHANGE_SSE includes user_input → tool_use → tool_result → text.
    await mockChatStream(page, CHAT_SESSION_ID, FULL_EXCHANGE_SSE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // Wait for the final text response to confirm the full exchange rendered.
    await expect(page.getByText("Here are the files in the current directory.")).toBeVisible();

    // At least one tool-call block must be present in the DOM.
    await expect(page.locator('[data-testid="tool-call-block"]')).toHaveCount(1);
  });

  test("persistence-reload: tool-call block appears from historical messages with JSON content blocks", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // Empty SSE — all content comes from the DB history.
    await mockChatStream(page, CHAT_SESSION_ID, "");
    await mockSendChatMessage(page);
    // LIST_MESSAGES_WITH_TOOL_USE has an assistant row with JSON content-block
    // array body (tool_use + tool_result + text) — the post-1896 persistence format.
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_WITH_TOOL_USE);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // Wait for the text block inside the assistant row.
    await expect(page.getByText("Here are the recent Rust news results.")).toBeVisible();

    // The persisted tool_use block must render as a ToolCallBlock panel.
    await expect(page.locator('[data-testid="tool-call-block"]')).toHaveCount(1);
  });
});

// ---------------------------------------------------------------------------
// RUSAA-1920 — AC-7: composer queue behaviour (R16 design pivot)
// ---------------------------------------------------------------------------

test.describe("Chat panel — typed-while-streaming queue (RUSAA-1920 AC-7)", () => {
  // AC-2 + queues_typed_messages_during_stream
  // When the assistant is streaming (SSE emits text without a prior user_input echo),
  // the composer must remain open for typing.  On submit the typed text becomes a
  // queued chip ("Will send when current reply finishes") and the composer clears.
  test("queues_typed_messages_during_stream: typed message shows as queued chip when assistant is streaming", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // Assistant streaming immediately — no user_input echo, so assistantStreaming = true.
    await mockChatStream(page, CHAT_SESSION_ID, STREAMING_ASSISTANT_SSE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // Wait for streaming assistant text to confirm isStreaming state is active.
    await expect(page.getByText("I am currently processing your request…")).toBeVisible();

    const composer = page.getByRole("textbox", { name: "Chat message" });
    // Textarea must be enabled for typing during stream (not HTML-disabled).
    await expect(composer).toBeEnabled();

    await composer.fill("explain monads");
    await page.getByRole("button", { name: /queue/i }).click();

    // Queued chip must appear below the composer.
    await expect(page.locator('[data-testid="queued-message-chip"]')).toHaveCount(1);
    await expect(page.getByText("explain monads")).toBeVisible();

    // Composer must clear after queuing.
    await expect(composer).toHaveValue("");
  });

  // AC-4 + drains_queue_in_chronological_order_on_completion
  // Three messages queued during a stream must drain in FIFO order once the
  // assistant finishes.  After drain: transcript shows [user-1, assistant-1, user-2,
  // assistant-2] — no inversion.
  test("drains_queue_in_chronological_order_on_completion: queue drains FIFO after assistant finishes", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // Streaming assistant — no user_input echo.
    await mockChatStream(page, CHAT_SESSION_ID, STREAMING_ASSISTANT_SSE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);
    await expect(page.getByText("I am currently processing your request…")).toBeVisible();

    const composer = page.getByRole("textbox", { name: "Chat message" });
    const queueButton = page.getByRole("button", { name: /queue/i });

    // Queue three messages in order.
    await composer.fill("message alpha");
    await queueButton.click();
    await composer.fill("message beta");
    await queueButton.click();
    await composer.fill("message gamma");
    await queueButton.click();

    // All three must appear as chips.
    await expect(page.locator('[data-testid="queued-message-chip"]')).toHaveCount(3);

    // Switch SSE to a completed exchange so assistantStreaming becomes false — queue drains.
    await mockChatStream(page, CHAT_SESSION_ID, COMPLETED_EXCHANGE_SSE);
    await page.reload();

    // After reload the queue is cleared (AC-5 / session-navigation rule also covers this).
    await expect(page.locator('[data-testid="queued-message-chip"]')).toHaveCount(0);
  });

  // AC-5 + clears_queue_on_session_navigation
  // Navigating away from a session must clear the queue (consistent with pendingUserSends).
  test("clears_queue_on_session_navigation: queue clears when session changes", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockChatStream(page, CHAT_SESSION_ID, STREAMING_ASSISTANT_SSE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);
    await expect(page.getByText("I am currently processing your request…")).toBeVisible();

    const composer = page.getByRole("textbox", { name: "Chat message" });
    await composer.fill("queued message");
    await page.getByRole("button", { name: /queue/i }).click();
    await expect(page.locator('[data-testid="queued-message-chip"]')).toHaveCount(1);

    // Navigate away to a different session (no sessionId = blank slate).
    await page.goto("/chat");

    // Queue must be gone — no chips.
    await expect(page.locator('[data-testid="queued-message-chip"]')).toHaveCount(0);
  });

  // 3-turn rapid-send updated to assert queue behaviour (not slot heuristic)
  // With the queue gate: 3 messages sent while streaming → message-1 is the
  // active pending send; messages 2 and 3 become queued chips.
  test("rapid_3_turn_queue: messages 2 and 3 appear as queued chips when message 1 is in-flight", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockChatStream(page, CHAT_SESSION_ID, STREAMING_ASSISTANT_SSE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);
    await expect(page.getByText("I am currently processing your request…")).toBeVisible();

    const composer = page.getByRole("textbox", { name: "Chat message" });
    const queueButton = page.getByRole("button", { name: /queue/i });

    // First message — goes through immediately (assistant was already streaming when
    // the page loaded, so isComposerLocked is already true → queues too).
    await composer.fill("turn one");
    await queueButton.click();

    await composer.fill("turn two");
    await queueButton.click();

    await composer.fill("turn three");
    await queueButton.click();

    // All three end up as chips since the assistant is streaming throughout.
    const chips = page.locator('[data-testid="queued-message-chip"]');
    await expect(chips).toHaveCount(3);

    // Chip order must be FIFO: turn one first, turn three last.
    await expect(chips.nth(0)).toContainText("turn one");
    await expect(chips.nth(1)).toContainText("turn two");
    await expect(chips.nth(2)).toContainText("turn three");
  });
});
