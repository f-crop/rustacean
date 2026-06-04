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
  MID_SEND_SSE,
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
    await expect(page.getByRole("button", { name: "Send" })).toBeVisible();
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
    // Existing session has a completed 1-turn history (user + assistant).
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_MCP_EXCHANGE);
    // SSE mock delivers a new exchange immediately on connect (simulates a
    // mid-send state where user_input + assistant text have arrived).
    await mockChatStream(page, CHAT_SESSION_ID, MID_SEND_SSE);
    await mockSendChatMessage(page);

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
