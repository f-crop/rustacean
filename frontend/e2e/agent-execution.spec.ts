import { expect, test, type Page } from "@playwright/test";
import {
  mockAuthenticatedSession,
  mockReposList,
  REPOS_EMPTY_RESPONSE,
} from "./fixtures/mock-api";

// ---------------------------------------------------------------------------
// Mock helpers
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

async function mockSseStream(page: Page): Promise<void> {
  await page.route("**/v1/ingest/events", async (route) => {
    await route.fulfill({
      status: 200,
      headers: {
        "Content-Type": "text/event-stream",
        "Cache-Control": "no-cache",
        Connection: "keep-alive",
      },
      body: "",
    });
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
// Tests
// ---------------------------------------------------------------------------

test.describe("Agent Execution — Activity page", () => {
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

  test("trace ID link navigates to trace viewer with correct URL", async ({ page }) => {
    await page.route("**/v1/ingestions/*/stages", (route) =>
      route.fulfill({
        json: {
          ingestion_run_id: "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
          trace_id: "0123456789abcdef0123456789abcdef",
          stages: [],
        },
      }),
    );
    await setupPage(page, RECENT_INGESTIONS_WITH_DATA);
    await page.goto("/activity");

    const traceLink = page.getByRole("link", { name: /^01234567/ });
    await expect(traceLink).toBeVisible();

    await traceLink.click();

    await expect(page).toHaveURL(/\/trace\/0123456789abcdef0123456789abcdef/);
    await expect(page).toHaveURL(/runId=a1b2c3d4-e5f6-7890-abcd-ef1234567890/);
  });
});
