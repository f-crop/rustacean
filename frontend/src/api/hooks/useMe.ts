import { useQuery, type UseQueryOptions } from "@tanstack/react-query";
import { apiClient, toApiError, type ApiError } from "../client";
import type { components } from "../generated/schema";

type MeResponse = components["schemas"]["MeResponse"];

export const meQueryKey = ["me"] as const;

export function useMe(
  options?: Omit<
    UseQueryOptions<MeResponse, ApiError>,
    "queryKey" | "queryFn"
  >,
) {
  return useQuery<MeResponse, ApiError>({
    queryKey: meQueryKey,
    queryFn: async () => {
      const { data, error, response } = await apiClient.GET("/v1/me");
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data;
    },
    ...options,
  });
}
