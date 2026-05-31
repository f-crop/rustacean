import { useQuery, useMutation, type UseQueryOptions } from "@tanstack/react-query";
import { apiClient, toApiError, type ApiError } from "../client";
import type { components } from "../generated/schema";

type ItemResponse = components["schemas"]["ItemResponse"];
type ModuleTreeResponse = components["schemas"]["ModuleTreeResponse"];
type SearchRequest = components["schemas"]["SearchRequest"];
type SearchResponse = components["schemas"]["SearchResponse"];
type SearchResult = components["schemas"]["SearchResult"];
type TraversalResponse = components["schemas"]["TraversalResponse"];
type TraversalNodeSchema = components["schemas"]["TraversalNodeSchema"];
type TraversalEdgeSchema = components["schemas"]["TraversalEdgeSchema"];

export type {
  ItemResponse,
  ModuleTreeResponse,
  SearchResult,
  TraversalResponse,
  TraversalNodeSchema,
  TraversalEdgeSchema,
};
export type { components as CodeIntelSchemas };

export function moduleTreeQueryKey(repoId: string) {
  return ["repos", repoId, "modules"] as const;
}

export function itemQueryKey(repoId: string, fqnB64: string) {
  return ["repos", repoId, "items", fqnB64] as const;
}

export function useModuleTree(
  repoId: string,
  options?: Omit<UseQueryOptions<ModuleTreeResponse, ApiError>, "queryKey" | "queryFn">,
) {
  return useQuery<ModuleTreeResponse, ApiError>({
    queryKey: moduleTreeQueryKey(repoId),
    queryFn: async () => {
      const { data, error, response } = await apiClient.GET(
        "/v1/repos/{repo_id}/modules",
        { params: { path: { repo_id: repoId } } },
      );
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data;
    },
    enabled: repoId.length > 0,
    staleTime: 60_000,
    ...options,
  });
}

export function useItem(
  repoId: string,
  fqnB64: string,
  options?: Omit<UseQueryOptions<ItemResponse, ApiError>, "queryKey" | "queryFn">,
) {
  return useQuery<ItemResponse, ApiError>({
    queryKey: itemQueryKey(repoId, fqnB64),
    queryFn: async () => {
      const { data, error, response } = await apiClient.GET(
        "/v1/repos/{repo_id}/items/{fqn_b64}",
        { params: { path: { repo_id: repoId, fqn_b64: fqnB64 } } },
      );
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data;
    },
    enabled: repoId.length > 0 && fqnB64.length > 0,
    ...options,
  });
}

export function fqnToB64(fqn: string): string {
  return btoa(fqn).replace(/\+/g, "-").replace(/\//g, "_").replace(/=/g, "");
}

export function b64ToFqn(b64: string): string {
  const padded = b64.replace(/-/g, "+").replace(/_/g, "/");
  const pad = padded.length % 4 === 0 ? "" : "=".repeat(4 - (padded.length % 4));
  return atob(padded + pad);
}

export function useSearch() {
  return useMutation<SearchResponse, ApiError, SearchRequest>({
    mutationFn: async (body) => {
      const { data, error, response } = await apiClient.POST("/v1/search", { body });
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data;
    },
  });
}

export function callersQueryKey(repoId: string, fqnB64: string) {
  return ["repos", repoId, "items", fqnB64, "callers"] as const;
}

export function calleesQueryKey(repoId: string, fqnB64: string) {
  return ["repos", repoId, "items", fqnB64, "callees"] as const;
}

export function useCallers(
  repoId: string,
  fqnB64: string,
  options?: Omit<UseQueryOptions<TraversalResponse, ApiError>, "queryKey" | "queryFn">,
) {
  return useQuery<TraversalResponse, ApiError>({
    queryKey: callersQueryKey(repoId, fqnB64),
    queryFn: async () => {
      const { data, error, response } = await apiClient.GET(
        "/v1/repos/{repo_id}/items/{fqn_b64}/callers",
        { params: { path: { repo_id: repoId, fqn_b64: fqnB64 } } },
      );
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data;
    },
    enabled: repoId.length > 0 && fqnB64.length > 0,
    ...options,
  });
}

export function useCallees(
  repoId: string,
  fqnB64: string,
  options?: Omit<UseQueryOptions<TraversalResponse, ApiError>, "queryKey" | "queryFn">,
) {
  return useQuery<TraversalResponse, ApiError>({
    queryKey: calleesQueryKey(repoId, fqnB64),
    queryFn: async () => {
      const { data, error, response } = await apiClient.GET(
        "/v1/repos/{repo_id}/items/{fqn_b64}/callees",
        { params: { path: { repo_id: repoId, fqn_b64: fqnB64 } } },
      );
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data;
    },
    enabled: repoId.length > 0 && fqnB64.length > 0,
    ...options,
  });
}
