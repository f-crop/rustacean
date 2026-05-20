import { useQuery, useQueryClient, type UseQueryOptions } from "@tanstack/react-query";
import { apiClient, toApiError, type ApiError } from "../client";
import type { components } from "../generated/schema";

export type RecentIngestionRun = components["schemas"]["RecentRunItem"];
export type RecentIngestionsResponse = components["schemas"]["RecentRunsResponse"];

export const recentIngestionsQueryKey = (tenantId: string) =>
  ["tenants", tenantId, "ingestions", "recent"] as const;

export function useRecentIngestions(
  tenantId: string,
  limit = 50,
  options?: Omit<
    UseQueryOptions<RecentIngestionsResponse, ApiError>,
    "queryKey" | "queryFn"
  >,
) {
  return useQuery<RecentIngestionsResponse, ApiError>({
    queryKey: recentIngestionsQueryKey(tenantId),
    queryFn: async () => {
      const { data, error, response } = await apiClient.GET("/v1/ingestions/recent", {
        params: { query: { limit } },
      });
      if (error || !data) {
        throw toApiError(response.status, error);
      }
      return data;
    },
    enabled: tenantId.length > 0,
    staleTime: 30_000,
    ...options,
  });
}

export function useInvalidateRecentIngestions() {
  const qc = useQueryClient();
  return (tenantId: string) => {
    // Cancel any in-flight polling requests first so a stale "running" response
    // cannot arrive after the fresh refetch and overwrite "succeeded" in cache.
    void qc.cancelQueries({ queryKey: recentIngestionsQueryKey(tenantId) });
    return qc.invalidateQueries({ queryKey: recentIngestionsQueryKey(tenantId) });
  };
}
