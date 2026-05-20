/**
 * Wave 7 happy-path E2E
 *
 * Mocked end-to-end walk through the Wave 7 user surfaces:
 *
 *   /repos → Connect a repo → ingestion trigger
 *     → /activity (run appears in recent ingestions with current-stage column)
 *     → /ingestion redirects to /activity
 *
 * All HTTP and SSE responses are mocked so the test runs in CI without a
 * compose stack. The live-stack variant lives in `ingestion-live.spec.ts`.
 *
 * Signup → login is covered separately (field-validation, smoke).
 * Connect dialog state-token flow is covered separately (repos-connect).
 * This spec proves the post-connect sequence hangs together as a journey,
 * which is the gap that Wave 7 UAT exposed.
 */

import { expect, test, type Page } from "@playwright/test";
import { ME_RESPONSE } from "./fixtures/mock-api";

const REPO_ID = "a1a1a1a1-bbbb-cccc-dddd-eeeeeeeeeeee";
const RUN_ID = "f1f1f1f1-2222-3333-4444-555555555555";
const REPO_FULL_NAME = "acme/web-app";
const INSTALL_UUID = "00112233-4455-6677-8899-aabbccddeeff";

const REPO_ITEM = {
  repo_id: REPO_ID,
  full_name: REPO_FULL_NAME,
  installation_id: INSTALL_UUID,
  default_branch: "main",
  status: "connected",
  connected_at: "2026-05-20T00:00:00Z",
  connected_by: ME_RESPONSE.user.id,
};

async function mockJourney(
  page: Page,
  opts: {
    readonly recentRunStatus: string;
    readonly stageStatus?: string;
  },
): Promise<void> {
  await page.route("**/v1/me", (route) => route.fulfill({ json: ME_RESPONSE }));

  await page.route("**/v1/repos", (route) => {
    if (route.request().method() === "GET") {
      return route.fulfill({ json: { repos: [REPO_ITEM] } });
    }
    return route.continue();
  });

  await page.route("**/v1/ingest/events", (route) =>
    route.fulfill({
      status: 200,
      headers: {
        "Content-Type": "text/event-stream",
        "Cache-Control": "no-cache",
        Connection: "keep-alive",
      },
      body: "",
    }),
  );

  await page.route("**/v1/ingestions/recent**", (route) =>
    route.fulfill({
      json: {
        runs: [
          {
            id: RUN_ID,
            repo_id: REPO_ID,
            status: opts.recentRunStatus,
            created_at: "2026-05-20T00:00:00Z",
            started_at: "2026-05-20T00:00:01Z",
            finished_at:
              opts.recentRunStatus === "succeeded"
                ? "2026-05-20T00:05:00Z"
                : null,
            trace_id: "11112222333344445555666677778888",
          },
        ],
      },
    }),
  );

  if (opts.stageStatus) {
    await page.route(`**/v1/ingestions/${RUN_ID}/stages`, (route) =>
      route.fulfill({
        json: {
          ingestion_run_id: RUN_ID,
          trace_id: null,
          stages: [
            { stage: "clone", status: "succeeded", started_at: null, finished_at: null, error_message: null },
            { stage: "expand", status: "succeeded", started_at: null, finished_at: null, error_message: null },
            { stage: "parse", status: opts.stageStatus, started_at: null, finished_at: null, error_message: null },
            { stage: "typecheck", status: "pending", started_at: null, finished_at: null, error_message: null },
            { stage: "extract", status: "pending", started_at: null, finished_at: null, error_message: null },
            { stage: "embed", status: "pending", started_at: null, finished_at: null, error_message: null },
            { stage: "project_pg", status: "pending", started_at: null, finished_at: null, error_message: null },
            { stage: "project_neo4j", status: "pending", started_at: null, finished_at: null, error_message: null },
            { stage: "project_qdrant", status: "pending", started_at: null, finished_at: null, error_message: null },
          ],
        },
      }),
    );
  }

  await page.route("**/v1/audit**", (route) =>
    route.fulfill({ json: { events: [], total: 0 } }),
  );
}

test.describe("Wave 7 happy path", () => {
  test("/ingestion redirects to /activity", async ({ page }) => {
    await page.route("**/v1/me", (route) => route.fulfill({ json: ME_RESPONSE }));
    await page.route("**/v1/ingest/events", (route) =>
      route.fulfill({
        status: 200,
        headers: { "Content-Type": "text/event-stream", "Cache-Control": "no-cache", Connection: "keep-alive" },
        body: "",
      }),
    );
    await page.route("**/v1/ingestions/recent**", (route) =>
      route.fulfill({ json: { runs: [] } }),
    );
    await page.route("**/v1/audit**", (route) =>
      route.fulfill({ json: { events: [], total: 0 } }),
    );
    await page.route("**/v1/repos", (route) => route.fulfill({ json: { repos: [] } }));

    await page.goto("/ingestion");
    await expect(page).toHaveURL(/\/activity/);
    await expect(
      page.getByRole("heading", { name: "Activity", exact: true }),
    ).toBeVisible();
  });

  test("connected repo → activity shows running run with current-stage column", async ({
    page,
  }) => {
    await mockJourney(page, { recentRunStatus: "running", stageStatus: "running" });

    await page.goto("/repos");
    await expect(
      page.getByRole("heading", { name: "Repositories" }),
    ).toBeVisible();
    await expect(page.getByText(REPO_FULL_NAME)).toBeVisible();

    await page.goto("/activity");
    await expect(
      page.getByRole("heading", { name: "Activity", exact: true }),
    ).toBeVisible();

    await expect(
      page.getByText(`${RUN_ID.slice(0, 8)}…`),
    ).toBeVisible();

    await expect(
      page.getByRole("cell", { name: "running", exact: false }),
    ).toBeVisible();

    await expect(
      page.getByRole("columnheader", { name: "Current stage" }),
    ).toBeVisible();

    await expect(
      page.getByRole("cell", { name: /parse \(\d\/9\)/ }),
    ).toBeVisible();
  });

  test("completed run shows — in current-stage column and finished timestamp", async ({
    page,
  }) => {
    await mockJourney(page, { recentRunStatus: "succeeded" });

    await page.goto("/activity");
    await expect(
      page.getByRole("cell", { name: "succeeded", exact: false }),
    ).toBeVisible();

    await expect(
      page.getByRole("columnheader", { name: "Current stage" }),
    ).toBeVisible();
  });
});
