import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";
import {
  mockAuthenticatedSession,
  mockReposList,
  REPOS_EMPTY_RESPONSE,
} from "./fixtures/mock-api";

const WCAG_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"];

// ---------------------------------------------------------------------------
// Mock data constants
// ---------------------------------------------------------------------------

const RECENT_INGESTIONS_EMPTY = { runs: [] };

const RECENT_INGESTIONS_WITH_DATA = {
  runs: [
    {
      id: "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
      repo_id: "repo-1",
      status: "succeeded",
      created_at: "2025-05-01T10:00:00Z",
      started_at: "2025-05-01T10:00:05Z",
      finished_at: "2025-05-01T10:02:30Z",
      trace_id: "0123456789abcdef0123456789abcdef",
    },
    {
      id: "b2c3d4e5-f6a7-8901-bcde-f12345678901",
      repo_id: "repo-2",
      status: "running",
      created_at: "2025-05-01T11:00:00Z",
      started_at: "2025-05-01T11:00:03Z",
      finished_at: null,
      trace_id: null,
    },
  ],
};

const AUDIT_EMPTY = { events: [], total: 0 };

// ---------------------------------------------------------------------------
// Mock helpers
// ---------------------------------------------------------------------------

async function mockSseStream(
  page: Page,
  events: string[] = [],
): Promise<void> {
  await page.route("**/v1/ingest/events", async (route) => {
    const body = events.join("") || "";
    await route.fulfill({
      status: 200,
      headers: {
        "Content-Type": "text/event-stream",
        "Cache-Control": "no-cache",
        Connection: "keep-alive",
      },
      body,
    });
  });
}

async function mockRecentIngestions(
  page: Page,
  response: typeof RECENT_INGESTIONS_EMPTY | typeof RECENT_INGESTIONS_WITH_DATA = RECENT_INGESTIONS_EMPTY,
): Promise<void> {
  await page.route("**/v1/ingestions/recent**", (route) =>
    route.fulfill({ json: response }),
  );
}

async function mockAuditEvents(
  page: Page,
  response: typeof AUDIT_EMPTY = AUDIT_EMPTY,
): Promise<void> {
  await page.route("**/v1/audit**", (route) => {
    if (route.request().method() === "GET") {
      return route.fulfill({ json: response });
    }
    return route.continue();
  });
}

async function setupPage(
  page: Page,
  ingestions: typeof RECENT_INGESTIONS_EMPTY | typeof RECENT_INGESTIONS_WITH_DATA = RECENT_INGESTIONS_EMPTY,
): Promise<void> {
  await mockAuthenticatedSession(page);
  await mockReposList(page, REPOS_EMPTY_RESPONSE);
  await mockRecentIngestions(page, ingestions);
  await mockAuditEvents(page);
  await mockSseStream(page);
}

// ---------------------------------------------------------------------------
// Tests: Agent Execution Viewer page (/agents/executions)
// ---------------------------------------------------------------------------

test.describe("Agent Execution — empty state", () => {
  test("renders heading and empty session history", async ({ page }) => {
    await setupPage(page);
    await page.goto("/agents/executions");

    await expect(
      page.getByRole("heading", { name: "Agent Execution" }),
    ).toBeVisible();

    await expect(
      page.getByText("No execution sessions found."),
    ).toBeVisible();
  });

  test("empty stream shows placeholder", async ({ page }) => {
    await setupPage(page);
    await page.goto("/agents/executions");

    await expect(
      page.getByText("No events yet. Events will appear here when an execution is running."),
    ).toBeVisible();
  });

  test("empty state a11y: no serious/critical axe violations", async ({
    page,
  }) => {
    await setupPage(page);
    await page.goto("/agents/executions");

    await expect(
      page.getByRole("heading", { name: "Agent Execution" }),
    ).toBeVisible();

    const results = await new AxeBuilder({ page })
      .withTags(WCAG_TAGS)
      .analyze();
    const violations = results.violations.filter(
      (v) => v.impact === "serious" || v.impact === "critical",
    );
    expect(
      violations,
      violations.map((v) => `${v.id}: ${v.description}`).join("\n"),
    ).toHaveLength(0);
  });
});

test.describe("Agent Execution — live events", () => {
  test("SSE event appears in the stream", async ({ page }) => {
    await mockAuthenticatedSession(page);
    await mockRecentIngestions(page);
    await mockSseStream(page, [
      "id: evt-1\n",
      "event: ingest.status\n",
      'data: {"ingest_request_id":"req-1","tenant_id":"tenant-1","status":"processing","error_message":"","occurred_at_ms":1700000001000}\n',
      "\n",
    ]);

    await page.goto("/agents/executions");

    await expect(page.getByText("ingest.status")).toBeVisible();
    await expect(page.getByText(/processing/)).toBeVisible();
  });

  test("session history table renders with data", async ({ page }) => {
    await mockAuthenticatedSession(page);
    await mockSseStream(page);
    await mockRecentIngestions(page, {
      runs: [
        {
          id: "run-001",
          repo_id: "repo-1",
          status: "succeeded",
          created_at: "2024-01-01T00:00:00Z",
          started_at: "2024-01-01T00:00:00Z",
          finished_at: "2024-01-01T00:01:00Z",
          trace_id: "trace-abc123",
        },
      ],
    });

    await page.goto("/agents/executions");

    const table = page.getByRole("table", { name: "Execution session history" });
    await expect(table).toBeVisible();
    await expect(page.getByText("run-001".slice(0, 8))).toBeVisible();
    await expect(page.getByText("succeeded")).toBeVisible();
  });
});

test.describe("Agent Execution — SSE error state", () => {
  test("shows disconnected indicator when SSE fails", async ({ page }) => {
    await mockAuthenticatedSession(page);
    await mockRecentIngestions(page);
    await page.route("**/v1/ingest/events", (route) =>
      route.fulfill({ status: 500, body: "Internal Server Error" }),
    );

    await page.goto("/agents/executions");

    await expect(page.getByText("Disconnected")).toBeVisible();
  });
});

test.describe("Agent Execution — auth gate", () => {
  test("shows sign-in prompt when not authenticated", async ({ page }) => {
    await page.route("**/v1/me", (route) => route.fulfill({ status: 401 }));
    await mockSseStream(page);

    await page.goto("/agents/executions");
    await expect(
      page.getByText("Sign in to view agent execution sessions."),
    ).toBeVisible();
  });
});

// ---------------------------------------------------------------------------
// Tests: Activity page (/activity)
// ---------------------------------------------------------------------------

test.describe("Activity page", () => {
  test("renders heading and empty session history", async ({ page }) => {
    await setupPage(page, RECENT_INGESTIONS_EMPTY);
    await page.goto("/activity");

    await expect(
      page.getByRole("heading", { name: "Activity", exact: true }),
    ).toBeVisible();

    await expect(
      page.getByText("No ingestion runs found."),
    ).toBeVisible();
  });

  test("session history table renders with data", async ({ page }) => {
    await setupPage(page, RECENT_INGESTIONS_WITH_DATA);
    await page.goto("/activity");

    await expect(
      page.getByRole("table", { name: "Recent ingestion runs" }),
    ).toBeVisible();

    await expect(
      page.getByText("a1b2c3d4"),
    ).toBeVisible();

    await expect(
      page.getByRole("table").getByText("succeeded"),
    ).toBeVisible();

    await expect(
      page.getByRole("table").getByText("running"),
    ).toBeVisible();
  });

  test("summary cards show correct values", async ({ page }) => {
    await setupPage(page, RECENT_INGESTIONS_WITH_DATA);
    await page.goto("/activity");

    const summary = page.locator('[aria-label="Summary metrics"]');
    await expect(summary).toBeVisible();

    await expect(summary.getByText("Connected repos")).toBeVisible();
    await expect(summary.getByText("0").first()).toBeVisible();
  });

  test("SSE status badge shows Offline when no live stream", async ({ page }) => {
    await setupPage(page);
    await page.goto("/activity");

    await expect(page.getByText("Offline")).toBeVisible();
  });

  test("audit chart section renders with empty data", async ({ page }) => {
    await setupPage(page);
    await page.goto("/activity");

    await expect(
      page.getByRole("heading", { name: "Audit events (last 14 days)" }),
    ).toBeVisible();

    await expect(
      page.getByRole("region", { name: "Audit events (last 14 days)" }),
    ).toBeVisible();
  });

  test("member activity section renders with empty state", async ({ page }) => {
    await setupPage(page);
    await page.goto("/activity");

    await expect(
      page.getByRole("heading", { name: "Member activity" }),
    ).toBeVisible();
  });

  test("recent queries section renders with empty state", async ({ page }) => {
    await setupPage(page);
    await page.goto("/activity");

    await expect(
      page.getByRole("heading", { name: "Recent queries" }),
    ).toBeVisible();
  });
});
