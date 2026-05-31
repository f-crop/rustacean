import {
  useMutation,
  useQuery,
  useQueryClient,
  type UseQueryOptions,
} from "@tanstack/react-query";
import { apiClient, toApiError, type ApiError } from "../client";
import type { components } from "../generated/schema";

type RepoItem = components["schemas"]["RepoItem"];
type ConnectedReposResponse = components["schemas"]["ConnectedReposResponse"];
type ConnectRepoRequest = components["schemas"]["ConnectRepoRequest"];
type ConnectRepoResponse = components["schemas"]["ConnectRepoResponse"];
type TriggerIngestResponse = components["schemas"]["TriggerIngestResponse"];

// AvailableRepo is the runtime shape returned by GET /v1/github/installations/{id}/available-repos.
// The generated schema incorrectly maps it to ListReposResponse (a naming collision in the backend).
export interface AvailableRepo {
  id: number;
  name: string;
  full_name: string;
  private: boolean;
  archived: boolean;
  default_branch: string;
  html_url: string;
}
export interface AvailableReposResponse {
  total_count: number;
  page: number;
  per_page: number;
  repositories: AvailableRepo[];
}

export type { RepoItem };

export const reposQueryKey = (tenantId: string) =>
  ["tenants", tenantId, "repos"] as const;

export const availableReposQueryKey = (installationId: string, page: number) =>
  ["github", "installations", installationId, "available-repos", page] as const;

export function useRepos(
  tenantId: string,
  options?: Omit<UseQueryOptions<ConnectedReposResponse, ApiError>, "queryKey" | "queryFn">,
) {
  return useQuery<ConnectedReposResponse, ApiError>({
    queryKey: reposQueryKey(tenantId),
    queryFn: async () => {
      const { data, error, response } = await apiClient.GET("/v1/repos");
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data;
    },
    enabled: tenantId.length > 0,
    ...options,
  });
}

export function useConnectRepo(tenantId: string) {
  const qc = useQueryClient();
  return useMutation<ConnectRepoResponse, ApiError, ConnectRepoRequest>({
    mutationFn: async (body) => {
      const { data, error, response } = await apiClient.POST("/v1/repos", { body });
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data;
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: reposQueryKey(tenantId) });
    },
  });
}

export function useTriggerIngest(tenantId: string) {
  const qc = useQueryClient();
  return useMutation<TriggerIngestResponse, ApiError, string>({
    mutationFn: async (repoId) => {
      const { data, error, response } = await apiClient.POST(
        "/v1/repos/{id}/ingest",
        { params: { path: { id: repoId } } },
      );
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data;
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: reposQueryKey(tenantId) });
    },
  });
}

export function useAvailableRepos(
  installationId: string,
  page = 1,
  options?: Omit<
    UseQueryOptions<AvailableReposResponse, ApiError>,
    "queryKey" | "queryFn"
  >,
) {
  return useQuery<AvailableReposResponse, ApiError>({
    queryKey: availableReposQueryKey(installationId, page),
    queryFn: async () => {
      // Cast needed: generated schema incorrectly maps this response to ListReposResponse
      // (name collision with repos::ListReposResponse). The actual runtime shape is AvailableReposResponse.
      const { data, error, response } = await apiClient.GET(
        "/v1/github/installations/{id}/available-repos",
        { params: { path: { id: installationId }, query: { page, per_page: 30 } } },
      );
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data as unknown as AvailableReposResponse;
    },
    enabled: installationId.length > 0,
    ...options,
  });
}
