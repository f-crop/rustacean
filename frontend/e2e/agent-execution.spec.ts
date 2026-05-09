import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import { mockAuthenticatedSession } from "./fixtures/mock-api";

const WCAG_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"];

async function mockSseStream(
  page: import("@playwright/test").Page,
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
  page: import("@playwright/test").Page,
  runs: unknown[] = [],
): Promise<void> {
  await page.route("**/v1/ingestions/recent*", (route) =>
    route.fulfill({ json: { runs } }),
  );
}

async function setupPage(page: import("@playwright/test").Page): Promise<void> {
  await mockAuthenticatedSession(page);
  await mockSseStream(page);
  await mockRecentIngestions(page);
}

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
    await mockRecentIngestions(page, [
      {
        id: "run-001",
        repo_id: "repo-1",
        status: "succeeded",
        created_at: "2024-01-01T00:00:00Z",
        started_at: "2024-01-01T00:00:00Z",
        finished_at: "2024-01-01T00:01:00Z",
        trace_id: "trace-abc123",
      },
    ]);

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
