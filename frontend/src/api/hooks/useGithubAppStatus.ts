import { useQuery, type UseQueryOptions } from "@tanstack/react-query";
import { apiClient, toApiError, type ApiError } from "../client";
import type { components } from "../generated/schema";

type AppStatusResponse = components["schemas"]["AppStatusResponse"];

export const githubAppStatusQueryKey = ["github-app-status"] as const;

export function useGithubAppStatus(
  options?: Omit<
    UseQueryOptions<AppStatusResponse, ApiError>,
    "queryKey" | "queryFn"
  >,
) {
  return useQuery<AppStatusResponse, ApiError>({
    queryKey: githubAppStatusQueryKey,
    queryFn: async () => {
      const { data, error, response } = await apiClient.GET(
        "/v1/admin/github/app-status",
      );
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data;
    },
    ...options,
  });
}
