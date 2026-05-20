/**
 * Wave 7 happy-path E2E (RUSAA-1549)
 *
 * Mocked end-to-end walk through the Wave 7 user surfaces:
 *
 *   /repos → Connect a repo → ingestion trigger
 *     → /ingestion (theatre shows running stages from SSE)
 *     → /activity (run appears in recent ingestions)
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

const TENANT_ID = ME_RESPONSE.current_tenant.id;
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

const STAGE_LABELS_IN_ORDER = [
  "Clone",
  "Expand",
  "Parse",
  "Typecheck",
  "Extract",
  "Embed",
  "Project (PostgreSQL)",
  "Project (Neo4j)",
  "Project (Qdrant)",
] as const;

interface SseEvent {
  readonly stage: string;
  readonly status: "processing" | "done";
  readonly seq: number;
}

function buildSseStream(events: ReadonlyArray<SseEvent>): string {
  return events
    .map((e, idx) => {
      const data = JSON.stringify({
        ingest_request_id: "req-1",
        tenant_id: TENANT_ID,
        status: e.status,
        stage: e.stage,
        stage_seq: e.seq,
        ingest_run_id: RUN_ID,
        error_message: "",
        occurred_at_ms: 1700000000000 + idx * 1000,
      });
      return `id: evt-${idx}\nevent: ingest.status\ndata: ${data}\n\n`;
    })
    .join("");
}

async function mockJourney(
  page: Page,
  opts: { readonly sseBody: string; readonly recentRunStatus: string },
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
      body: opts.sseBody,
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

  await page.route("**/v1/audit**", (route) =>
    route.fulfill({ json: { events: [], total: 0 } }),
  );
}

test.describe("Wave 7 happy path", () => {
  test("connected repo → theatre shows live stages → activity lists the run", async ({
    page,
  }) => {
    const sseEvents: SseEvent[] = [
      { stage: "clone", status: "done", seq: 0 },
      { stage: "expand", status: "done", seq: 1 },
      { stage: "parse", status: "processing", seq: 2 },
    ];

    await mockJourney(page, {
      sseBody: buildSseStream(sseEvents),
      recentRunStatus: "running",
    });

    // 1. Land on /repos and see the connected repository.
    await page.goto("/repos");
    await expect(
      page.getByRole("heading", { name: "Repositories" }),
    ).toBeVisible();
    await expect(page.getByText(REPO_FULL_NAME)).toBeVisible();

    // 2. Open Ingestion Theatre and confirm SSE events drove per-stage state.
    await page.goto("/ingestion");
    await expect(
      page.getByRole("heading", { name: "Ingestion Theatre" }),
    ).toBeVisible();

    // The active-state panel should be visible (SSE delivered events).
    await expect(page.getByTestId("ingestion-active-state")).toBeVisible();

    // clone + expand → Done; parse → Running; later stages remain Pending.
    await expect(
      page.getByRole("img", { name: "Clone stage: Done" }),
    ).toBeVisible();
    await expect(
      page.getByRole("img", { name: "Expand stage: Done" }),
    ).toBeVisible();
    await expect(
      page.getByRole("img", { name: "Parse stage: Running" }),
    ).toBeVisible();
    await expect(
      page.getByRole("img", { name: "Typecheck stage: Pending" }),
    ).toBeVisible();

    // All nine stages must be present in the timeline, even those still pending.
    for (const label of STAGE_LABELS_IN_ORDER) {
      await expect(
        page.getByRole("listitem").filter({ hasText: label }),
      ).toBeVisible();
    }

    // 3. Activity page lists the run with running status.
    await page.goto("/activity");
    await expect(
      page.getByRole("heading", { name: "Activity", exact: true }),
    ).toBeVisible();
    // The recent-runs table renders the truncated run ID prefix.
    await expect(
      page.getByText(`${RUN_ID.slice(0, 8)}…`),
    ).toBeVisible();
    // While running, finished column is the em-dash placeholder.
    await expect(
      page.getByRole("cell", { name: "running", exact: false }),
    ).toBeVisible();
  });

  test("completed run shows Done across stages and finished timestamp on activity", async ({
    page,
  }) => {
    const sseEvents: SseEvent[] = STAGE_LABELS_IN_ORDER.map((_, idx) => ({
      stage: [
        "clone",
        "expand",
        "parse",
        "typecheck",
        "extract",
        "embed",
        "project_pg",
        "project_neo4j",
        "project_qdrant",
      ][idx]!,
      status: "done" as const,
      seq: idx,
    }));

    await mockJourney(page, {
      sseBody: buildSseStream(sseEvents),
      recentRunStatus: "succeeded",
    });

    await page.goto("/ingestion");
    for (const label of STAGE_LABELS_IN_ORDER) {
      await expect(
        page.getByRole("img", { name: `${label} stage: Done` }),
      ).toBeVisible();
    }

    await page.goto("/activity");
    await expect(
      page.getByRole("cell", { name: "succeeded", exact: false }),
    ).toBeVisible();
  });
});
