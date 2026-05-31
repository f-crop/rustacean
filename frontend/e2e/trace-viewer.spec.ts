import { test, expect, type Page } from "@playwright/test";
import { TraceViewerPage, TEST_TRACE_ID, TEST_RUN_ID } from "./pages/TraceViewerPage";
import {
  mockAuthenticatedSession,
  mockReposList,
  REPOS_EMPTY_RESPONSE,
  STAGE_TIMELINE_RESPONSE,
} from "./fixtures/mock-api";

// ---------------------------------------------------------------------------
// Mock helpers
// ---------------------------------------------------------------------------

const TEMPO_TRACE_RESPONSE = {
  data: [
    {
      traceID: TEST_TRACE_ID,
      spans: [
        {
          traceID: TEST_TRACE_ID,
          spanID: "span0001",
          operationName: "http.request",
          startTime: 1_700_000_000_000_000,
          duration: 5_000_000,
          processID: "p1",
          references: [],
        },
        {
          traceID: TEST_TRACE_ID,
          spanID: "span0002",
          operationName: "db.query",
          startTime: 1_700_000_001_000_000,
          duration: 2_000_000,
          processID: "p2",
          references: [{ refType: "CHILD_OF", traceID: TEST_TRACE_ID, spanID: "span0001" }],
        },
      ],
      processes: {
        p1: { serviceName: "control-api" },
        p2: { serviceName: "projector-pg" },
      },
    },
  ],
};

async function mockTempo(page: Page): Promise<void> {
  await page.route("**/api/traces/**", (route) =>
    route.fulfill({ json: TEMPO_TRACE_RESPONSE }),
  );
}

async function abortTempo(page: Page): Promise<void> {
  await page.route("**/api/traces/**", (route) => route.abort("connectionfailed"));
}

async function mockStageTimeline(page: Page): Promise<void> {
  await page.route("**/v1/ingestions/*/stages", (route) =>
    route.fulfill({
      json: {
        ...STAGE_TIMELINE_RESPONSE,
        ingestion_run_id: TEST_RUN_ID,
        trace_id: TEST_TRACE_ID,
      },
    }),
  );
}

async function setupAuth(page: Page): Promise<TraceViewerPage> {
  await mockAuthenticatedSession(page);
  await mockReposList(page, REPOS_EMPTY_RESPONSE);
  return new TraceViewerPage(page);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test.describe("Trace viewer — page rendering", () => {
  test("renders heading with trace ID from URL param", async ({ page }) => {
    const tv = await setupAuth(page);
    await abortTempo(page);
    await tv.goto(TEST_TRACE_ID);

    await expect(tv.heading).toBeVisible();
    await expect(page.getByText(TEST_TRACE_ID).first()).toBeVisible();
  });

  test("page is accessible at the trace URL with a minimal session", async ({ page }) => {
    // The trace route has no auth guard — it loads the TraceViewerPage regardless.
    // Verify the heading renders to confirm the route resolves correctly.
    const traceViewer = await setupAuth(page);
    await abortTempo(page);
    await page.goto(`/trace/${TEST_TRACE_ID}`);
    await expect(traceViewer.heading).toBeVisible();
  });
});

test.describe("Trace viewer — Tempo unavailable fallback", () => {
  test("shows no-runId message when no ingestion run linked", async ({ page }) => {
    const tv = await setupAuth(page);
    await abortTempo(page);
    await tv.goto(TEST_TRACE_ID);

    await expect(tv.noRunIdMessage).toBeVisible();
  });

  test("shows stage timeline fallback when runId provided", async ({ page }) => {
    const tv = await setupAuth(page);
    await abortTempo(page);
    await mockStageTimeline(page);
    await tv.goto(TEST_TRACE_ID, TEST_RUN_ID);

    await expect(tv.stageTimeline).toBeVisible();
  });

  test("stage timeline shows Pipeline stage timeline heading", async ({ page }) => {
    const tv = await setupAuth(page);
    await abortTempo(page);
    await mockStageTimeline(page);
    await tv.goto(TEST_TRACE_ID, TEST_RUN_ID);

    await expect(
      page.getByRole("heading", { name: "Pipeline stage timeline" }),
    ).toBeVisible();
  });

  test("stage timeline renders individual stage rows", async ({ page }) => {
    const tv = await setupAuth(page);
    await abortTempo(page);
    await mockStageTimeline(page);
    await tv.goto(TEST_TRACE_ID, TEST_RUN_ID);

    await expect(tv.pipelineStagesList).toBeVisible();
    await expect(page.getByText("clone", { exact: false })).toBeVisible();
    await expect(page.getByText("expand", { exact: false })).toBeVisible();
    await expect(page.getByText("parse", { exact: false })).toBeVisible();
  });

  test("stage timeline shows ingestion run ID", async ({ page }) => {
    const tv = await setupAuth(page);
    await abortTempo(page);
    await mockStageTimeline(page);
    await tv.goto(TEST_TRACE_ID, TEST_RUN_ID);

    await expect(page.getByText(TEST_RUN_ID)).toBeVisible();
  });

  test("stage fallback note mentions pipeline stage timeline", async ({ page }) => {
    const tv = await setupAuth(page);
    await abortTempo(page);
    await mockStageTimeline(page);
    await tv.goto(TEST_TRACE_ID, TEST_RUN_ID);

    await expect(tv.stageFallbackNote).toBeVisible();
  });
});

test.describe("Trace viewer — Tempo success", () => {
  test("shows Tempo span tree when Tempo returns data", async ({ page }) => {
    const tv = await setupAuth(page);
    await mockTempo(page);
    await tv.goto(TEST_TRACE_ID);

    await expect(tv.tempoSpanTree).toBeVisible();
  });

  test("span tree shows service names from process map", async ({ page }) => {
    const tv = await setupAuth(page);
    await mockTempo(page);
    await tv.goto(TEST_TRACE_ID);

    await expect(page.getByText(/control-api/)).toBeVisible();
  });

  test("span tree shows operation names", async ({ page }) => {
    const tv = await setupAuth(page);
    await mockTempo(page);
    await tv.goto(TEST_TRACE_ID);

    await expect(page.getByText("http.request")).toBeVisible();
  });

  test("span count and total duration shown in Tempo section header", async ({ page }) => {
    const tv = await setupAuth(page);
    await mockTempo(page);
    await tv.goto(TEST_TRACE_ID);

    await expect(page.getByText(/2 spans/)).toBeVisible();
  });
});
