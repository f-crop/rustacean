import { type Locator, type Page } from "@playwright/test";

export const TEST_TRACE_ID = "abc123def456abc123def456abc123de";
export const TEST_RUN_ID = "run-id-trace-test-00000001";

export class TraceViewerPage {
  readonly heading: Locator;
  readonly tempoSpanTree: Locator;
  readonly stageTimeline: Locator;
  readonly pipelineStagesList: Locator;
  readonly loadingStatus: Locator;
  readonly noRunIdMessage: Locator;
  readonly stageFallbackNote: Locator;

  constructor(private readonly page: Page) {
    this.heading = page.getByRole("heading", { name: "Trace viewer", exact: true });
    this.tempoSpanTree = page.getByRole("region", { name: "Tempo span tree" });
    this.stageTimeline = page.getByRole("region", { name: "Stage timeline" });
    this.pipelineStagesList = page.getByRole("list", { name: "Pipeline stages" });
    this.loadingStatus = page.getByRole("status");
    this.noRunIdMessage = page.getByText(
      "Tempo unavailable and no ingestion run linked to this trace ID.",
    );
    this.stageFallbackNote = page.getByText(/Showing pipeline stage timeline/);
  }

  async goto(traceId: string, runId?: string): Promise<void> {
    const search = runId ? `?runId=${encodeURIComponent(runId)}` : "";
    await this.page.goto(`/trace/${traceId}${search}`);
  }
}
