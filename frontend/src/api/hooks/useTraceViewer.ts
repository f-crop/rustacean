import { useQuery, type UseQueryOptions } from "@tanstack/react-query";
import { apiClient, toApiError, type ApiError } from "../client";
import type { components } from "../generated/schema";

export type StageTimelineResponse = components["schemas"]["StageTimelineResponse"];
export type StageRunItem = components["schemas"]["StageRunItem"];

export function stageTimelineQueryKey(ingestionRunId: string) {
  return ["ingestions", ingestionRunId, "stages"] as const;
}

export function useStageTimeline(
  ingestionRunId: string,
  options?: Omit<UseQueryOptions<StageTimelineResponse, ApiError>, "queryKey" | "queryFn">,
) {
  return useQuery<StageTimelineResponse, ApiError>({
    queryKey: stageTimelineQueryKey(ingestionRunId),
    queryFn: async () => {
      const { data, error, response } = await apiClient.GET(
        "/v1/ingestions/{ingestion_run_id}/stages",
        { params: { path: { ingestion_run_id: ingestionRunId } } },
      );
      if (error || !data) {
        throw toApiError(response.status, error);
      }
      return data;
    },
    enabled: ingestionRunId.length > 0,
    ...options,
  });
}
