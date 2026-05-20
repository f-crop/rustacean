/**
 * Regression test for RUSAA-1631: ingestion stage display skips intermediate stages.
 *
 * Root cause: `refetchInterval: 8_000` in useIngestionStages caused stage polls to
 * miss fast-completing stages. The mock here advances the stage API response on each
 * call to simulate the full 9-stage pipeline, then asserts ≥6 of 9 stages were
 * visible in the Activity page stage cell during the run.
 */

import { expect, test, type Page } from "@playwright/test";
import { ME_RESPONSE } from "./fixtures/mock-api";

const RUN_ID = "b2b2b2b2-3333-4444-5555-666666666666";
const REPO_ID = "a1a1a1a1-bbbb-cccc-dddd-eeeeeeeeeeee";

const PIPELINE_STAGES = [
  "clone",
  "expand",
  "parse",
  "typecheck",
  "extract",
  "embed",
  "project_pg",
  "project_neo4j",
  "project_qdrant",
] as const;

type PipelineStage = typeof PIPELINE_STAGES[number];

function buildStagesResponse(activeIdx: number) {
  return {
    ingestion_run_id: RUN_ID,
    trace_id: null,
    stages: PIPELINE_STAGES.map((s, i) => ({
      stage: s,
      status: i < activeIdx ? "succeeded" : i === activeIdx ? "running" : "pending",
      started_at: null,
      finished_at: null,
      error_message: null,
    })),
  };
}

async function setupMocks(
  page: Page,
  opts: {
    getRunStatus: () => string;
    getStageCallCount: () => number;
    incrementStageCallCount: () => void;
  },
): Promise<void> {
  await page.route("**/v1/me", (route) => route.fulfill({ json: ME_RESPONSE }));
  await page.route("**/v1/repos", (route) =>
    route.fulfill({ json: { repos: [] } }),
  );
  await page.route("**/v1/audit**", (route) =>
    route.fulfill({ json: { events: [], total: 0 } }),
  );
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
            status: opts.getRunStatus(),
            created_at: "2026-05-21T00:00:00Z",
            started_at: "2026-05-21T00:00:01Z",
            finished_at: opts.getRunStatus() === "succeeded" ? "2026-05-21T00:01:00Z" : null,
            trace_id: null,
          },
        ],
      },
    }),
  );

  await page.route(`**/v1/ingestions/${RUN_ID}/stages`, (route) => {
    const callIdx = opts.getStageCallCount();
    opts.incrementStageCallCount();
    const activeIdx = Math.min(callIdx, PIPELINE_STAGES.length - 1);
    return route.fulfill({ json: buildStagesResponse(activeIdx) });
  });
}

test.describe("Stage progression regression (RUSAA-1631)", () => {
  test("≥6 of 9 pipeline stages are observable in the Activity stage cell during a run", async ({
    page,
  }) => {
    let stageCallCount = 0;
    let runStatus = "running";

    await setupMocks(page, {
      getRunStatus: () => runStatus,
      getStageCallCount: () => stageCallCount,
      incrementStageCallCount: () => { stageCallCount++; },
    });

    await page.goto("/activity");
    await expect(
      page.getByRole("heading", { name: "Activity", exact: true }),
    ).toBeVisible();

    const observedStages = new Set<PipelineStage>();
    const stageCell = page.getByTestId("stage-cell").first();

    // Poll the stage cell for up to 20 seconds, collecting each unique stage label.
    // At refetchInterval=1500ms, we get ~13 polls in 20s — enough to traverse all 9 stages.
    const deadline = Date.now() + 20_000;
    while (Date.now() < deadline) {
      const text = await stageCell.textContent({ timeout: 1_000 }).catch(() => null);
      if (text && text !== "—") {
        const match = text.match(/^(\w+)/);
        if (match) {
          const stage = match[1] as PipelineStage;
          if ((PIPELINE_STAGES as readonly string[]).includes(stage)) {
            observedStages.add(stage);
          }
        }
      }
      // Short dwell so we don't busy-spin but still catch 1.5s-interval updates
      await page.waitForTimeout(300);

      // Once we've seen enough stages, flip the run to terminal so the test ends cleanly
      if (observedStages.size >= 6 && runStatus === "running") {
        runStatus = "succeeded";
      }
      if (runStatus === "succeeded" && observedStages.size >= 6) {
        break;
      }
    }

    expect(
      observedStages.size,
      `Expected ≥6 of 9 stages to be observed; got ${observedStages.size}: [${[...observedStages].join(", ")}]`,
    ).toBeGreaterThanOrEqual(6);
  });

  test("stage cell retains last-seen label while run is still active (no premature blank)", async ({
    page,
  }) => {
    let stageCallCount = 0;
    const runStatus = "running";

    await setupMocks(page, {
      getRunStatus: () => runStatus,
      getStageCallCount: () => stageCallCount,
      incrementStageCallCount: () => { stageCallCount++; },
    });

    await page.goto("/activity");
    await expect(
      page.getByRole("heading", { name: "Activity", exact: true }),
    ).toBeVisible();

    const stageCell = page.getByTestId("stage-cell").first();

    // Wait for any stage to appear (confirms stage polling is working)
    await expect(stageCell).not.toHaveText("—", { timeout: 5_000 });

    // Capture the current label
    const firstLabel = await stageCell.textContent({ timeout: 1_000 });
    expect(firstLabel).toBeTruthy();

    // Verify the cell does not briefly blank to "—" between polls (wait 2 full poll cycles)
    // If the cell ever shows "—" while the run is still "running", the regression is present.
    let sawBlank = false;
    const check = Date.now() + 4_000;
    while (Date.now() < check) {
      const text = await stageCell.textContent({ timeout: 500 }).catch(() => null);
      if (text === "—") {
        sawBlank = true;
        break;
      }
      await page.waitForTimeout(200);
    }

    expect(sawBlank, "Stage cell should not blank out while run is still active").toBe(false);
  });
});
