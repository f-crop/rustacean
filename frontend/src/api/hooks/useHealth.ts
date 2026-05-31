import { useQuery, type UseQueryOptions } from "@tanstack/react-query";
import { apiClient, toApiError, type ApiError } from "../client";
import type { components } from "../generated/schema";

type ProbeResponse = components["schemas"]["ProbeResponse"];

export const healthQueryKey = ["health"] as const;

export function useHealth(
  options?: Omit<
    UseQueryOptions<ProbeResponse, ApiError>,
    "queryKey" | "queryFn"
  >,
) {
  return useQuery<ProbeResponse, ApiError>({
    queryKey: healthQueryKey,
    queryFn: async () => {
      const result = await apiClient.GET("/health");
      const status = result.response.status;
      if (!result.response.ok || !result.data) {
        throw toApiError(status, result.error ?? null, result.response);
      }
      return result.data;
    },
    ...options,
  });
}
