import { expect, test } from "@playwright/test";
import {
  mockAuthenticatedSession,
  mockReposList,
  REPOS_EMPTY_RESPONSE,
} from "./fixtures/mock-api";
import {
  SESSION_ID,
  SESSION_ID_RUNNING,
  SESSION_ID_PENDING,
  SESSION_RUNNING,
  HISTORY_EVENTS_RUNNING,
  mockSessionList,
  mockCompletedSession,
  mockPendingSession,
  mockRunningSession,
} from "./session-replay.fixtures";

// ---------------------------------------------------------------------------
// Tests: Session list — regression
// ---------------------------------------------------------------------------

test.describe("Session list — /agents/executions", () => {
  test("renders heading and session history table", async ({ page }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockSessionList(page);
    await page.goto("/agents/executions");

    await expect(
      page.getByRole("heading", { name: "Agent Execution", level: 1 }),
    ).toBeVisible();
    await expect(
      page.getByRole("table", { name: "Execution session history" }),
    ).toBeVisible();
  });

  test("shows both sessions with correct status and runtime badges", async ({ page }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockSessionList(page);
    await page.goto("/agents/executions");

    const table = page.getByRole("table", { name: "Execution session history" });
    await expect(table).toBeVisible();

    // Session IDs (first 8 chars shown as links)
    await expect(table.getByText("aaaabbbb")).toBeVisible();
    await expect(table.getByText("11112222")).toBeVisible();

    // Status cells
    await expect(table.getByText("succeeded")).toBeVisible();
    await expect(table.getByText("running")).toBeVisible();

    // Runtime badges
    await expect(table.getByText("Claude Code")).toBeVisible();
    await expect(table.getByText("OpenCode")).toBeVisible();
  });

  test("empty state shows no-sessions message", async ({ page }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await page.route("**/v1/agents/sessions", (route) =>
      route.fulfill({ json: { sessions: [] } }),
    );
    await page.goto("/agents/executions");

    await expect(page.getByText("No execution sessions found.")).toBeVisible();
  });

  test("session row link navigates to replay page", async ({ page }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockSessionList(page);
    await mockCompletedSession(page);
    await page.goto("/agents/executions");

    // The link renders the first 8 chars of the session ID
    await page.getByRole("link", { name: /aaaabbbb/ }).first().click();

    await expect(page).toHaveURL(new RegExp(`/agents/${SESSION_ID}`));
    await expect(
      page.getByRole("heading", { name: "Session Replay", level: 1 }),
    ).toBeVisible();
  });
});

// ---------------------------------------------------------------------------
// Tests: Session replay — completed session
// ---------------------------------------------------------------------------

test.describe("Session replay — completed session", () => {
  test.beforeEach(async ({ page }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockCompletedSession(page);
  });

  test("renders Session Replay heading with session ID", async ({ page }) => {
    await page.goto(`/agents/${SESSION_ID}`);

    await expect(
      page.getByRole("heading", { name: "Session Replay", level: 1 }),
    ).toBeVisible();
    // Session ID rendered as mono text with title attribute
    await expect(page.getByTitle(SESSION_ID)).toBeVisible();
  });

  test("shows session metadata: runtime, status badge, prompt preview", async ({ page }) => {
    await page.goto(`/agents/${SESSION_ID}`);

    // Status badge in page header
    await expect(page.getByText("succeeded")).toBeVisible();
    // Runtime label in metadata grid
    await expect(page.getByText("Claude Code")).toBeVisible();
    // Tokens label present (value format varies by locale)
    await expect(page.getByText("Tokens", { exact: true })).toBeVisible();
    // Prompt preview
    await expect(page.getByText("Build a REST API")).toBeVisible();
  });

  test("renders all six filter chips", async ({ page }) => {
    await page.goto(`/agents/${SESSION_ID}`);

    const filterGroup = page.getByRole("group", { name: "Filter by event type" });
    await expect(filterGroup).toBeVisible();

    for (const label of [
      "Text",
      "Tool Use",
      "Tool Result",
      "Thinking",
      "Error",
      "Lifecycle",
    ]) {
      await expect(
        filterGroup.getByRole("button", { name: label }),
      ).toBeVisible();
    }
  });

  test("events section shows total count after history loads", async ({ page }) => {
    await page.goto(`/agents/${SESSION_ID}`);

    // 3 history events (text + tool_use + tool_result)
    await expect(page.locator('h2[id="events-heading"]')).toContainText("3");
  });

  test("filter chip narrows event count; Clear restores all", async ({ page }) => {
    await page.goto(`/agents/${SESSION_ID}`);

    const heading = page.locator('h2[id="events-heading"]');
    await expect(heading).toContainText("3");

    const filterGroup = page.getByRole("group", { name: "Filter by event type" });
    const toolUseBtn = filterGroup.getByRole("button", { name: "Tool Use" });

    await expect(toolUseBtn).toHaveAttribute("aria-pressed", "false");
    await toolUseBtn.click();
    await expect(toolUseBtn).toHaveAttribute("aria-pressed", "true");

    // Only the 1 tool_use event passes; total remains 3
    await expect(heading).toContainText("1 / 3");

    // Clear button appears when any filter is active
    await page.getByRole("button", { name: "Clear" }).click();
    await expect(toolUseBtn).toHaveAttribute("aria-pressed", "false");
    await expect(heading).toContainText("3");
    // "1 / 3" form must be gone (all events shown again)
    await expect(heading).not.toContainText("1 / 3");
  });

  test("breadcrumb links back to session list", async ({ page }) => {
    await page.goto(`/agents/${SESSION_ID}`);

    const breadcrumb = page.getByRole("navigation", { name: "Breadcrumb" });
    await expect(breadcrumb).toBeVisible();
    await expect(breadcrumb.getByText("← Back to sessions")).toBeVisible();

    await breadcrumb.getByRole("link").click();
    await expect(page).toHaveURL(/\/agents\/executions/);
  });

  test("Download NDJSON button triggers file download with correct filename", async ({ page }) => {
    await page.goto(`/agents/${SESSION_ID}`);

    const downloadBtn = page.getByRole("button", { name: "Download NDJSON log" });
    await expect(downloadBtn).toBeVisible();

    const [download] = await Promise.all([
      page.waitForEvent("download"),
      downloadBtn.click(),
    ]);

    expect(download.suggestedFilename()).toBe(`session-${SESSION_ID}.ndjson`);
  });
});

// ---------------------------------------------------------------------------
// Tests: Live session view — running session with SSE
// ---------------------------------------------------------------------------

test.describe("Live session view — running session", () => {
  test.beforeEach(async ({ page }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockRunningSession(page);
  });

  test("shows running status badge and SSE connection indicator", async ({ page }) => {
    await page.goto(`/agents/${SESSION_ID_RUNNING}`);

    await expect(page.getByText("running")).toBeVisible();
    // SSE indicator appears once history is fully loaded and session is running
    await expect(
      page.getByText(/^(Connecting…|Live|Disconnected)$/),
    ).toBeVisible();
  });

  test("live SSE event appends to history events without gap", async ({ page }) => {
    await page.goto(`/agents/${SESSION_ID_RUNNING}`);

    // History has seq 1-3; SSE delivers seq 4 → total 4 events
    await expect(page.locator('h2[id="events-heading"]')).toContainText("4");
  });
});

// ---------------------------------------------------------------------------
// Tests: History-join — deduplication and sequence continuity
// ---------------------------------------------------------------------------

test.describe("History join — sequence continuity", () => {
  test("merged event list contains all sequences without gaps", async ({ page }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockRunningSession(page);
    await page.goto(`/agents/${SESSION_ID_RUNNING}`);

    // History: seq 1,2,3. SSE: seq 4. No gaps → 4 events total.
    await expect(page.locator('h2[id="events-heading"]')).toContainText("4");
  });

  test("SSE events with sequence ≤ last history seq are deduplicated", async ({ page }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);

    // SSE re-delivers seq 3 (already in history) AND seq 4 (new)
    const duplicateSseBody = [
      "event: session.event",
      `data: ${JSON.stringify({
        sequence: 3,
        event_type: "text",
        payload: { type: "text", text: "Duplicate seq 3 — must be filtered" },
      })}`,
      "",
      "event: session.event",
      `data: ${JSON.stringify({
        sequence: 4,
        event_type: "text",
        payload: { type: "text", text: "New seq 4 — must appear" },
      })}`,
      "",
      "",
    ].join("\n");

    await page.route(
      `**/v1/agents/sessions/${SESSION_ID_RUNNING}**`,
      (route) => {
        const url = route.request().url();
        if (url.includes("/events/history")) {
          return route.fulfill({
            json: { events: HISTORY_EVENTS_RUNNING, next_seq: null },
          });
        }
        if (url.includes("/events")) {
          return route.fulfill({
            status: 200,
            headers: {
              "Content-Type": "text/event-stream",
              "Cache-Control": "no-cache",
            },
            body: duplicateSseBody,
          });
        }
        return route.fulfill({ json: SESSION_RUNNING });
      },
    );

    await page.goto(`/agents/${SESSION_ID_RUNNING}`);

    // Seq 3 from SSE is filtered (maxHistSeq = 3, only seq > 3 passes).
    // Only seq 4 is added → total 4 events, not 5.
    await expect(page.locator('h2[id="events-heading"]')).toContainText("4");
    await expect(page.locator('h2[id="events-heading"]')).not.toContainText("5");
  });
});

// ---------------------------------------------------------------------------
// Tests: Pending session — graceful empty state (RUSAA-1382)
// ---------------------------------------------------------------------------

test.describe("Pending session — graceful empty state", () => {
  test.beforeEach(async ({ page }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);
    await mockPendingSession(page);
  });

  test("shows pending status badge and prompt preview", async ({ page }) => {
    await page.goto(`/agents/${SESSION_ID_PENDING}`);

    await expect(page.getByText("pending")).toBeVisible();
    await expect(page.getByText("what is 2+2")).toBeVisible();
  });

  test('shows "No events to display." instead of error message', async ({ page }) => {
    await page.goto(`/agents/${SESSION_ID_PENDING}`);

    await expect(page.getByText("No events to display.")).toBeVisible();
    await expect(page.getByText("Could not load event history.")).not.toBeVisible();
  });

  test("Download NDJSON button is disabled for pending sessions", async ({ page }) => {
    await page.goto(`/agents/${SESSION_ID_PENDING}`);

    const downloadBtn = page.getByRole("button", { name: "Download NDJSON log" });
    await expect(downloadBtn).toBeVisible();
    await expect(downloadBtn).toBeDisabled();
    await expect(downloadBtn).toHaveAttribute(
      "title",
      "No events yet — session is pending",
    );
  });
});
