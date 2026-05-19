import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import { mockAuthenticatedSession } from "./fixtures/mock-api";

const WCAG_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"];

async function mockSseStream(
  page: import("@playwright/test").Page,
  events: string[] = [],
): Promise<void> {
  await page.route("**/v1/ingest/events", async (route) => {
    await route.fulfill({
      status: 200,
      headers: {
        "Content-Type": "text/event-stream",
        "Cache-Control": "no-cache",
        Connection: "keep-alive",
      },
      body: events.join(""),
    });
  });
}

async function setupPage(page: import("@playwright/test").Page): Promise<void> {
  await mockAuthenticatedSession(page);
  await mockSseStream(page);
}

const STAGE_LABELS: Array<[stage: string, label: string]> = [
  ["clone", "Clone"],
  ["expand", "Expand"],
  ["parse", "Parse"],
  ["typecheck", "Typecheck"],
  ["extract", "Extract"],
  ["embed", "Embed"],
  ["project_pg", "Project (PostgreSQL)"],
  ["project_neo4j", "Project (Neo4j)"],
  ["project_qdrant", "Project (Qdrant)"],
];

test.describe("Ingestion Theatre — empty state", () => {
  test("renders empty state when no events on the bus", async ({ page }) => {
    await setupPage(page);
    await page.goto("/ingestion");

    await expect(
      page.getByRole("heading", { name: "Ingestion Theatre" }),
    ).toBeVisible();

    const emptyState = page.getByTestId("ingestion-empty-state");
    await expect(emptyState).toBeVisible();
    await expect(
      emptyState.getByText("No ingestion in progress"),
    ).toBeVisible();
  });

  test("empty state shows all 9 pipeline stages as pending", async ({
    page,
  }) => {
    await setupPage(page);
    await page.goto("/ingestion");

    for (const [, label] of STAGE_LABELS) {
      await expect(
        page.getByRole("listitem").filter({ hasText: label }),
      ).toBeVisible();
    }
  });

  test("empty state a11y: no serious/critical axe violations", async ({
    page,
  }) => {
    await setupPage(page);
    await page.goto("/ingestion");
    await expect(
      page.getByRole("heading", { name: "Ingestion Theatre" }),
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

test.describe("Ingestion Theatre — live events", () => {
  test("processing event flips clone stage to running", async ({ page }) => {
    await mockAuthenticatedSession(page);
    await mockSseStream(page, [
      "id: evt-1\n",
      "event: ingest.status\n",
      'data: {"ingest_request_id":"req-1","tenant_id":"tenant-1","status":"processing","stage":"clone","stage_seq":0,"ingest_run_id":"run-1","error_message":"","occurred_at_ms":1700000001000}\n',
      "\n",
    ]);

    await page.goto("/ingestion");
    await expect(page.getByTestId("ingestion-active-state")).toBeVisible();

    const cloneItem = page.getByRole("listitem").filter({ hasText: "Clone" });
    await expect(cloneItem).toBeVisible();
    await expect(
      cloneItem.getByRole("img", { name: "Clone stage: Running" }),
    ).toBeVisible();
  });

  test("done events per stage show each stage as done", async ({ page }) => {
    await mockAuthenticatedSession(page);
    const stageEvents: string[] = STAGE_LABELS.flatMap(([stage], i) => [
      `id: evt-${i + 1}\n`,
      `event: ingest.status\n`,
      `data: {"ingest_request_id":"req-1","tenant_id":"tenant-1","status":"done","stage":"${stage}","stage_seq":${i},"ingest_run_id":"run-1","error_message":"","occurred_at_ms":${1700000002000 + i}}\n`,
      `\n`,
    ]);
    await mockSseStream(page, stageEvents);

    await page.goto("/ingestion");
    await expect(page.getByTestId("ingestion-active-state")).toBeVisible();

    for (const [, label] of STAGE_LABELS) {
      const item = page.getByRole("listitem").filter({ hasText: label });
      await expect(
        item.getByRole("img", { name: `${label} stage: Done` }),
      ).toBeVisible();
    }
  });

  test("populated state a11y: no serious/critical axe violations", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockSseStream(page, [
      "id: evt-3\n",
      "event: ingest.status\n",
      'data: {"ingest_request_id":"req-1","tenant_id":"tenant-1","status":"processing","stage":"clone","stage_seq":0,"ingest_run_id":"run-1","error_message":"","occurred_at_ms":1700000003000}\n',
      "\n",
    ]);

    await page.goto("/ingestion");
    await expect(page.getByTestId("ingestion-active-state")).toBeVisible();

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

test.describe("Ingestion Theatre — auth gate", () => {
  test("redirects to login when not authenticated", async ({ page }) => {
    await page.route("**/v1/me", (route) => route.fulfill({ status: 401 }));
    await mockSseStream(page);

    await page.goto("/ingestion");
    await expect(page.locator("text=Sign in to view ingestion progress")).toBeVisible();
  });
});
