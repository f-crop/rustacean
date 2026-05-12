import {
  useMutation,
  useQuery,
  useQueryClient,
  type UseQueryOptions,
} from "@tanstack/react-query";
import { apiClient, toApiError, type ApiError } from "../client";
import type { components } from "../generated/schema";

type ListSessionsResponse = components["schemas"]["ListSessionsResponse"];
type SessionItem = components["schemas"]["SessionItem"];
type SessionDetail = components["schemas"]["SessionDetail"];
type CreateSessionRequest = components["schemas"]["CreateSessionRequest"];
type CreateSessionResponse = components["schemas"]["CreateSessionResponse"];

// Tenant-scoped query key prevents stale rows from a previous tenant flashing
// while the active tenant's refetch is in flight.
export const agentSessionsQueryKey = (tenantId: string) =>
  ["tenants", tenantId, "agent-sessions"] as const;

export function useAgentSessions(
  tenantId: string,
  options?: Omit<
    UseQueryOptions<ListSessionsResponse, ApiError>,
    "queryKey" | "queryFn"
  >,
) {
  return useQuery<ListSessionsResponse, ApiError>({
    queryKey: agentSessionsQueryKey(tenantId),
    queryFn: async () => {
      const { data, error, response } = await apiClient.GET(
        "/v1/agents/sessions",
      );
      if (error || !data) {
        throw toApiError(response.status, error);
      }
      return data;
    },
    enabled: tenantId.length > 0,
    staleTime: 15_000,
    ...options,
  });
}

export function useCreateSession(tenantId: string) {
  const qc = useQueryClient();
  return useMutation<CreateSessionResponse, ApiError, CreateSessionRequest>({
    mutationFn: async (body) => {
      const { data, error, response } = await apiClient.POST(
        "/v1/agents/sessions",
        { body },
      );
      if (error || !data) {
        throw toApiError(response.status, error);
      }
      return data;
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: agentSessionsQueryKey(tenantId) });
    },
  });
}

export function useDeleteSession(tenantId: string) {
  const qc = useQueryClient();
  return useMutation<void, ApiError, string>({
    mutationFn: async (id) => {
      const { error, response } = await apiClient.DELETE(
        "/v1/agents/sessions/{id}",
        { params: { path: { id } } },
      );
      if (error) {
        throw toApiError(response.status, error);
      }
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: agentSessionsQueryKey(tenantId) });
    },
  });
}

export function useSessionDetail(
  tenantId: string,
  sessionId: string,
  options?: Omit<
    UseQueryOptions<SessionDetail, ApiError>,
    "queryKey" | "queryFn"
  >,
) {
  return useQuery<SessionDetail, ApiError>({
    queryKey: [...agentSessionsQueryKey(tenantId), sessionId] as const,
    queryFn: async () => {
      const { data, error, response } = await apiClient.GET(
        "/v1/agents/sessions/{id}",
        { params: { path: { id: sessionId } } },
      );
      if (error || !data) {
        throw toApiError(response.status, error);
      }
      return data;
    },
    enabled: tenantId.length > 0 && sessionId.length > 0,
    ...options,
  });
}

export type {
  ListSessionsResponse,
  SessionItem,
  SessionDetail,
  CreateSessionRequest,
  CreateSessionResponse,
};
