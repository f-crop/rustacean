import { useQueries } from "@tanstack/react-query";
import { apiClient, toApiError } from "../client";
import type { components } from "../generated/schema";

export type StageRunItem = components["schemas"]["StageRunItem"];
export type StageTimelineResponse = components["schemas"]["StageTimelineResponse"];

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

const TOTAL_STAGES = PIPELINE_STAGES.length;

export const ingestionStagesQueryKey = (runId: string) =>
  ["ingestions", runId, "stages"] as const;

export function currentStageLabel(stages: readonly StageRunItem[]): string | null {
  const running = stages.find((s) => s.status === "running");
  if (!running) return null;
  const seq = (PIPELINE_STAGES as readonly string[]).indexOf(running.stage);
  return `${running.stage} (${seq === -1 ? "?" : seq + 1}/${TOTAL_STAGES})`;
}

export function useIngestionStagesForRunningRuns(
  runIds: readonly string[],
): Record<string, string> {
  const results = useQueries({
    queries: runIds.map((runId) => ({
      queryKey: ingestionStagesQueryKey(runId),
      queryFn: async (): Promise<StageTimelineResponse> => {
        const { data, error, response } = await apiClient.GET(
          "/v1/ingestions/{ingestion_run_id}/stages",
          { params: { path: { ingestion_run_id: runId } } },
        );
        if (error || !data) {
          throw toApiError(response.status, error);
        }
        return data;
      },
      staleTime: 1_000,
      refetchInterval: 1_500,
    })),
  });

  const map: Record<string, string> = {};
  runIds.forEach((runId, i) => {
    const result = results[i];
    if (result?.data) {
      const label = currentStageLabel(result.data.stages);
      if (label) map[runId] = label;
    }
  });
  return map;
}
