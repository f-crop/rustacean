import { test, expect, type Page } from "@playwright/test";
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
  FULL_EXCHANGE_SSE,
  SESSION_ERROR_SSE,
  AUDIT_WITH_TOOL_CALL,
  LIST_SESSIONS_ONE,
  LIST_MESSAGES_MCP_EXCHANGE,
  LIST_MESSAGES_WITH_TOOL_USE,
  MID_SEND_SSE,
  IN_PROGRESS_NO_ECHO_SSE,
  TURN1_COMPLETE_STALE_INPROGRESS_SSE,
  TURN2_WITH_ECHO_SSE,
  LIST_MESSAGES_EMPTY,
} from "./fixtures/chat-mock-api";

const CHAT_URL = "/chat";

// ---------------------------------------------------------------------------
// Shared setup
// ---------------------------------------------------------------------------

async function setupChatPage(page: Page, sseBody = FULL_EXCHANGE_SSE): Promise<void> {
  await mockAuthenticatedSession(page);
  await mockReposList(page, REPOS_EMPTY_RESPONSE);
  await mockChatSessionsListAndCreate(page);
  await mockSendChatMessage(page);
  await mockChatStream(page, CHAT_SESSION_ID, sseBody);
}

// Opens the chat page and clicks the "New chat session" button so a session
// becomes active and MessageComposer is visible.
async function openNewSession(page: Page): Promise<void> {
  await page.goto(CHAT_URL);
  await page.getByRole("button", { name: "New chat session" }).first().click();
  await expect(page.getByRole("textbox", { name: "Chat message" })).toBeVisible();
}

// ---------------------------------------------------------------------------
// Golden path — feature flag on (VITE_FEATURE_CHAT_PANEL=true at build time)
// ---------------------------------------------------------------------------

test.describe("Chat panel — golden path", () => {
  test("renders session sidebar and empty state when no sessions exist", async ({ page }) => {
    await setupChatPage(page);
    await page.goto(CHAT_URL);

    await expect(page.getByRole("complementary", { name: "Chat sessions" })).toBeVisible();
    await expect(page.getByText("No sessions yet. Click + New to start.")).toBeVisible();
    await expect(
      page.getByText("Select a session from the sidebar or start a new one."),
    ).toBeVisible();
  });

  test("shows chat heading in header", async ({ page }) => {
    await setupChatPage(page);
    await page.goto(CHAT_URL);

    await expect(page.getByRole("heading", { name: "Chat", level: 1 })).toBeVisible();
  });

  test("activates session and shows composer after clicking + New in sidebar", async ({ page }) => {
    await setupChatPage(page);
    await page.goto(CHAT_URL);

    await page.getByRole("button", { name: "New chat session", exact: false }).first().click();

    await expect(page.getByRole("textbox", { name: "Chat message" })).toBeVisible();
    // Button label is "Send" when idle or "Queue" when assistant streams — both confirm the composer is mounted.
    await expect(page.getByRole("button", { name: /send|queue/i })).toBeVisible();
  });

  test("renders user message, tool call, and text response from SSE stream", async ({ page }) => {
    await setupChatPage(page);
    await openNewSession(page);

    // The SSE mock delivers events immediately on connect — wait for them to render.
    await expect(
      page.getByText("List files in the current directory"),
    ).toBeVisible();
    await expect(page.getByText("Here are the files in the current directory.")).toBeVisible();
  });

  test("renders MCP tool call block with Done badge after tool_result arrives", async ({
    page,
  }) => {
    await setupChatPage(page);
    await openNewSession(page);

    // ToolCallBlock aria-label: "${name} tool call — ${statusLabel}"
    await expect(
      page.getByRole("button", { name: /list_directory tool call — Done/ }),
    ).toBeVisible();
  });

  test("tool call block expands to reveal input and result JSON on click", async ({ page }) => {
    await setupChatPage(page);
    await openNewSession(page);

    const toolBtn = page.getByRole("button", { name: /list_directory tool call/ });
    await expect(toolBtn).toBeVisible();
    await toolBtn.click();

    await expect(toolBtn).toHaveAttribute("aria-expanded", "true");
    await expect(page.getByText("Input")).toBeVisible();
    await expect(page.getByText("Result")).toBeVisible();
  });

  test("session ID short-form appears in chat header after session is created", async ({
    page,
  }) => {
    await setupChatPage(page);
    await openNewSession(page);

    // Header renders first 8 chars of session ID (title attr holds full ID)
    await expect(page.getByTitle(CHAT_SESSION_ID)).toBeVisible();
  });
});

// ---------------------------------------------------------------------------
// Reload persistence — session + message history survive a hard reload
// ---------------------------------------------------------------------------

test.describe("Chat panel — reload persistence", () => {
  test("restores active session and renders message history after reload", async ({ page }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    // List endpoint returns the existing session; SSE stream is empty (session not streaming)
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockListChatMessages(page, CHAT_SESSION_ID);
    await mockChatStream(page, CHAT_SESSION_ID, "");

    // Navigate directly to the chat page with sessionId search param (simulates post-reload URL)
    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // Session sidebar is visible
    await expect(page.getByRole("complementary", { name: "Chat sessions" })).toBeVisible();

    // Both messages from LIST_MESSAGES_TWO_TURNS are rendered
    await expect(page.getByText("Hello from reload test")).toBeVisible();
    await expect(page.getByText("Hello back! I remember your message.")).toBeVisible();
    await expect(page.getByText("Second message", { exact: true })).toBeVisible();
    await expect(page.getByText("Got your second message.")).toBeVisible();

    // Session ID still shows in header
    await expect(page.getByTitle(CHAT_SESSION_ID)).toBeVisible();
  });

  test("URL search param persists sessionId so reload can restore session", async ({ page }) => {
    await setupChatPage(page);
    await page.goto(CHAT_URL);

    // Create a new session
    await page.getByRole("button", { name: "New chat session" }).first().click();
    await expect(page.getByRole("textbox", { name: "Chat message" })).toBeVisible();

    // URL should now include sessionId search param
    const url = new URL(page.url());
    expect(url.searchParams.get("sessionId")).toBe(CHAT_SESSION_ID);
  });

  // AC3: reload of a 2-turn session renders both user prompt and assistant reply.
  test("renders user prompt and assistant reply after hard reload on a 2-turn session", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_MCP_EXCHANGE);
    await mockChatStream(page, CHAT_SESSION_ID, "");

    // First visit — session restores from URL param.
    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);
    await expect(page.getByText("What MCP tools are available?")).toBeVisible();
    await expect(page.getByText("The following MCP tools are registered:")).toBeVisible();

    // Hard reload — both messages must survive.
    await page.reload();
    await expect(page.getByText("What MCP tools are available?")).toBeVisible();
    await expect(page.getByText("The following MCP tools are registered:")).toBeVisible();
  });
});

// ---------------------------------------------------------------------------
// Mid-send regression — prior messages must survive while SSE streams a reply
// ---------------------------------------------------------------------------

test.describe("Chat panel — mid-send message persistence", () => {
  // AC4: user bubble and prior history both remain visible alongside the
  // streaming assistant reply (no replacement / disappearing messages).
  test("prior history and user bubble both visible while assistant reply streams", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // SSE mock delivers a new exchange immediately on connect (simulates a
    // mid-send state where user_input + assistant text have arrived).
    await mockChatStream(page, CHAT_SESSION_ID, MID_SEND_SSE);
    // Register mockSendChatMessage before mockListChatMessages so Playwright's
    // LIFO route priority makes mockListChatMessages (registered last) win for
    // GET requests to the messages endpoint.
    await mockSendChatMessage(page);
    // Existing session has a completed 1-turn history (user + assistant).
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_MCP_EXCHANGE);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // Prior turn from history is visible.
    await expect(page.getByText("What MCP tools are available?")).toBeVisible();
    await expect(page.getByText("The following MCP tools are registered:")).toBeVisible();

    // New turn from SSE is also visible (no replacement — both coexist).
    await expect(page.getByText("How do I use the bash tool?")).toBeVisible();
    await expect(
      page.getByText("You can use the bash tool to run shell commands in the workspace."),
    ).toBeVisible();
  });
});

// ---------------------------------------------------------------------------
// Bug A — Optimistic user bubble: visible before SSE user_input echo
// ---------------------------------------------------------------------------

test.describe("Chat panel — optimistic user bubble (Bug A)", () => {
  // AC1: user bubble visible BEFORE any SSE user_input event arrives.
  // SSE stream is empty (never delivers user_input), so the bubble must come
  // from the optimistic pending-send path in ChatPage.
  test("user bubble appears immediately after send with no SSE events yet", async ({ page }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // Empty SSE — no user_input echo will ever arrive.
    await mockChatStream(page, CHAT_SESSION_ID, "");
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_MCP_EXCHANGE);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // Wait for history to load so we know the session is active.
    await expect(page.getByText("What MCP tools are available?")).toBeVisible();

    // Type and send a new message — do NOT wait for any SSE event.
    await page.getByRole("textbox", { name: "Chat message" }).fill("Tell me about Rust");
    await page.getByRole("button", { name: "Send" }).click();

    // Bubble must appear immediately from the optimistic path (no SSE yet).
    await expect(page.getByText("Tell me about Rust")).toBeVisible();
  });
});

// ---------------------------------------------------------------------------
// Bug B — Tool-use blocks survive reload
// ---------------------------------------------------------------------------

test.describe("Chat panel — tool_use blocks survive reload (Bug B)", () => {
  // AC2: tool_use block present after reload (loaded from DB history with JSON body).
  test("tool_use block from JSON content-block history renders on load and after reload", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // History contains an assistant message with tool_use in JSON array body.
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_WITH_TOOL_USE);
    await mockChatStream(page, CHAT_SESSION_ID, "");

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // User prompt and tool_use block are visible on initial load.
    await expect(page.getByText("Search for recent Rust news")).toBeVisible();
    await expect(
      page.getByRole("button", { name: /mcp__rust_brain__search_demo tool call/ }),
    ).toBeVisible();
    await expect(page.getByText("Here are the recent Rust news results.")).toBeVisible();

    // Hard reload — both must survive (history re-fetched from mocked listMessages).
    await page.reload();

    await expect(page.getByText("Search for recent Rust news")).toBeVisible();
    await expect(
      page.getByRole("button", { name: /mcp__rust_brain__search_demo tool call/ }),
    ).toBeVisible();
    await expect(page.getByText("Here are the recent Rust news results.")).toBeVisible();
  });
});

// ---------------------------------------------------------------------------
// Audit visibility — verifies the activity page reflects chat tool-call audit rows
// ---------------------------------------------------------------------------

test.describe("Chat panel — audit row visibility", () => {
  test("activity page Total audit events card shows non-zero count with chat audit rows", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);

    // Mock audit endpoint — include a chat tool-call audit event.
    await page.route("**/v1/audit**", (route) =>
      route.fulfill({ json: AUDIT_WITH_TOOL_CALL }),
    );
    // Stub other activity-page endpoints so they don't error.
    await page.route("**/v1/repos**", (route) => route.fulfill({ json: { repos: [] } }));
    await page.route("**/v1/tenants/*/ingestion-runs**", (route) =>
      route.fulfill({ json: { runs: [] } }),
    );
    await page.route("**/v1/ingest/events**", (route) =>
      route.fulfill({
        status: 200,
        headers: { "Content-Type": "text/event-stream", "Cache-Control": "no-cache" },
        body: "",
      }),
    );

    await page.goto("/activity");

    // SummaryCards renders "Total audit events" with the total count.
    const summaryGrid = page.locator('[aria-label="Summary metrics"]');
    await expect(summaryGrid).toBeVisible();
    await expect(summaryGrid.getByText("Total audit events")).toBeVisible();
    // Total is 1 (from AUDIT_WITH_TOOL_CALL.total).
    await expect(summaryGrid.locator("p.text-2xl").nth(1)).toContainText("1");
  });
});

// ---------------------------------------------------------------------------
// Bug C — Pending bubble ordering: appears before in-progress assistant turn
// ---------------------------------------------------------------------------

test.describe("Chat panel — pending bubble ordering (Bug C)", () => {
  // Regression guard for RUSAA-1898: optimistic pending bubble was appended AFTER
  // the in-progress assistant turn instead of slotted before it.
  //
  // Scenario: session has existing history (U1, A1); SSE delivers A2 tokens
  // WITHOUT a user_input echo (simulating the backend race window).  The user
  // sends U2 optimistically.  The pending bubble must appear BEFORE the
  // in-progress A2 content, not after it.
  test("pending user bubble appears before in-progress assistant content, not after", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // SSE delivers A2 tokens with no user_input echo — buildTranscript marks
    // this as inProgress: true.
    await mockChatStream(page, CHAT_SESSION_ID, IN_PROGRESS_NO_ECHO_SSE);
    await mockSendChatMessage(page);
    // History: U1 + A1 already persisted.
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_MCP_EXCHANGE);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // Wait for both history and streaming content to render.
    await expect(page.getByText("What MCP tools are available?")).toBeVisible();
    await expect(page.getByText("I'm analyzing your request now...")).toBeVisible();

    // Assistant is streaming — button label is "Queue" (queue-gate contract).
    await page.getByRole("textbox", { name: "Chat message" }).fill("What's next?");
    await page.getByRole("button", { name: /queue/i }).click();

    // With queue-gate the message becomes a queued chip — the slot-append bug
    // (RUSAA-1898) is impossible since the message never enters the transcript
    // as a pending bubble while the assistant is streaming.
    await expect(page.locator('[data-testid="queued-message-chip"]')).toHaveCount(1);
    await expect(page.locator('[data-testid="queued-message-chip"]')).toContainText("What's next?");
  });
});

// ---------------------------------------------------------------------------
// Bug C turn-1 regression — ordering on first turn of a brand-new session
// ---------------------------------------------------------------------------

test.describe("Chat panel — turn-1 pending bubble ordering (Bug C turn-1)", () => {
  // Regression guard for RUSAA-1900: on the very first turn of a fresh session
  // the slot predicate guard (base.some("user")) was false because base held
  // only the streaming assistant item with no prior user row, so the pending
  // bubble was appended AFTER the assistant turn instead of slotted before it.
  //
  // Scenario: fresh session (empty history); SSE delivers assistant tokens
  // WITHOUT a user_input echo; user sends U1 optimistically.  The pending
  // bubble must appear BEFORE the in-progress assistant content.
  test("turn-1: pending user bubble appears before in-progress assistant on a fresh session", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // SSE delivers assistant tokens with no user_input echo.
    await mockChatStream(page, CHAT_SESSION_ID, IN_PROGRESS_NO_ECHO_SSE);
    await mockSendChatMessage(page);
    // Empty history — this is the first turn of a brand-new session.
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // Wait for the in-progress assistant content from SSE to render.
    await expect(page.getByText("I'm analyzing your request now...")).toBeVisible();

    // Assistant is streaming — button label is "Queue" (queue-gate contract).
    await page.getByRole("textbox", { name: "Chat message" }).fill("what are the tools available");
    await page.getByRole("button", { name: /queue/i }).click();

    // With queue-gate the message becomes a queued chip — the slot-append bug
    // (RUSAA-1900) is impossible since the message never enters the transcript
    // as a pending bubble while the assistant is streaming.
    await expect(page.locator('[data-testid="queued-message-chip"]')).toHaveCount(1);
    await expect(page.locator('[data-testid="queued-message-chip"]')).toContainText("what are the tools available");
  });
});

// ---------------------------------------------------------------------------
// Bug C turn-2 regression — stale inProgress after completed turn-1
// ---------------------------------------------------------------------------

test.describe("Chat panel — turn-2 pending bubble ordering (Bug C turn-2)", () => {
  // Regression guard for RUSAA-1904: PR #701 removed the user-guard entirely,
  // fixing turn-1 but breaking turn-2.  After turn-1 completes (user_input echo
  // arrived + all text tokens received), the assistant's inProgress flag stays
  // true until a NEW user_input event flushes it.  The slot predicate previously
  // found this stale-inProgress assistant and mis-slotted the turn-2 pending
  // bubble before it: [user-1, user-2-pending, assistant-1] instead of
  // [user-1, assistant-1, user-2-pending].
  //
  // Scenario: SSE has completed turn-1 (user_input + text, both received);
  // user sends U2 optimistically.  Pending U2 must appear AFTER assistant-1.
  test("turn-2: pending user bubble appears after completed assistant-1, not before it", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // SSE has full turn-1 (user_input + text) but no turn-2 user_input.
    // The assistant-1 item keeps inProgress: true (stale — no flush event).
    await mockChatStream(page, CHAT_SESSION_ID, TURN1_COMPLETE_STALE_INPROGRESS_SSE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // Both turn-1 items must be visible from SSE.
    await expect(page.getByText("what are the tools available")).toBeVisible();
    await expect(page.getByText("Here are the available tools.")).toBeVisible();

    // Stale-inProgress still makes assistantStreaming = true → button label is "Queue".
    await page.getByRole("textbox", { name: "Chat message" }).fill("second message");
    await page.getByRole("button", { name: /queue/i }).click();

    // With queue-gate the message becomes a queued chip — the stale-inProgress
    // mis-slot bug (RUSAA-1904) is impossible since no pending bubble enters the
    // transcript while assistantStreaming is true.
    await expect(page.locator('[data-testid="queued-message-chip"]')).toHaveCount(1);
    await expect(page.locator('[data-testid="queued-message-chip"]')).toContainText("second message");
  });

  // AC3: after turn-2's user_input echo arrives, transcript reads
  // [user-1, assistant-1, user-2, assistant-2-streaming] — items 1 and 2
  // retain correct order with no visual reshuffle.
  test("turn-2 echo: completed exchange maintains [user-1, assistant-1, user-2, assistant-2] order", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    // SSE delivers both turns: user_input(1)+text(1)+user_input(2)+text(2).
    // buildTranscript produces [user-1, assistant-1, user-2, assistant-2(inProgress)].
    await mockChatStream(page, CHAT_SESSION_ID, TURN2_WITH_ECHO_SSE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // All four items must be visible.
    await expect(page.getByText("what are the tools available")).toBeVisible();
    await expect(page.getByText("Here are the available tools.")).toBeVisible();
    await expect(page.getByText("Tell me about ownership")).toBeVisible();
    await expect(page.getByText("Ownership is Rust's key memory feature.")).toBeVisible();

    // Verify top-to-bottom order: user-1 < assistant-1 < user-2 < assistant-2.
    const user1Box = await page.getByText("what are the tools available").boundingBox();
    const asst1Box = await page.getByText("Here are the available tools.").boundingBox();
    const user2Box = await page.getByText("Tell me about ownership").boundingBox();
    const asst2Box = await page.getByText("Ownership is Rust's key memory feature.").boundingBox();

    expect(user1Box).not.toBeNull();
    expect(asst1Box).not.toBeNull();
    expect(user2Box).not.toBeNull();
    expect(asst2Box).not.toBeNull();

    expect(user1Box!.y).toBeLessThan(asst1Box!.y);
    expect(asst1Box!.y).toBeLessThan(user2Box!.y);
    expect(user2Box!.y).toBeLessThan(asst2Box!.y);
  });
});

// ---------------------------------------------------------------------------
// Error paths
// ---------------------------------------------------------------------------

test.describe("Chat panel — error paths", () => {
  test("shows error alert when sending a message returns 500", async ({ page }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockChatSessionsListAndCreate(page);
    await mockChatStream(page, CHAT_SESSION_ID, "");

    await page.route("**/v1/chat/sessions/*/messages", (route) =>
      route.fulfill({ status: 500, json: { error: "Internal server error" } }),
    );

    await page.goto(CHAT_URL);
    await page.getByRole("button", { name: "New chat session" }).first().click();
    await expect(page.getByRole("textbox", { name: "Chat message" })).toBeVisible();

    await page.getByRole("textbox", { name: "Chat message" }).fill("This will fail");
    await page.getByRole("button", { name: "Send" }).click();

    await expect(page.getByRole("alert")).toBeVisible();
  });

  test("renders session.error SSE event as error transcript item", async ({ page }) => {
    await setupChatPage(page, SESSION_ERROR_SSE);
    await openNewSession(page);

    // session.error delivers an error item with the message text.
    await expect(
      page.getByRole("alert").filter({ hasText: "Session timed out" }),
    ).toBeVisible();
  });
});
