import { useQuery, type UseQueryOptions } from "@tanstack/react-query";
import { apiClient, toApiError, type ApiError } from "../client";
import type { components } from "../generated/schema";

export type SessionItem = components["schemas"]["SessionItem"];
export type ListSessionsResponse = components["schemas"]["ListSessionsResponse"];

export const agentSessionsQueryKey = ["agents", "sessions"] as const;

export function useAgentSessions(
  options?: Omit<
    UseQueryOptions<ListSessionsResponse, ApiError>,
    "queryKey" | "queryFn"
  >,
) {
  return useQuery<ListSessionsResponse, ApiError>({
    queryKey: agentSessionsQueryKey,
    queryFn: async () => {
      const { data, error, response } = await apiClient.GET("/v1/agents/sessions");
      if (error || !data) {
        throw toApiError(response.status, error);
      }
      return data;
    },
    staleTime: 30_000,
    ...options,
  });
}
