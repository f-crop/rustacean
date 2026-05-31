import {
  useMutation,
  useQuery,
  useQueryClient,
  type UseQueryOptions,
} from "@tanstack/react-query";
import { apiClient, toApiError, type ApiError } from "../client";
import type { components } from "../generated/schema";

type ListApiKeysResponse = components["schemas"]["ListApiKeysResponse"];
type CreateApiKeyRequest = components["schemas"]["CreateApiKeyRequest"];
type CreateApiKeyResponse = components["schemas"]["CreateApiKeyResponse"];

// Tenant-scoped key prevents stale rows from a previous tenant flashing
// in `/api-keys` while the active tenant's refetch is in flight.
export const apiKeysQueryKey = (tenantId: string) =>
  ["tenants", tenantId, "api-keys"] as const;

export function useApiKeys(
  tenantId: string,
  options?: Omit<
    UseQueryOptions<ListApiKeysResponse, ApiError>,
    "queryKey" | "queryFn"
  >,
) {
  return useQuery<ListApiKeysResponse, ApiError>({
    queryKey: apiKeysQueryKey(tenantId),
    queryFn: async () => {
      const { data, error, response } = await apiClient.GET("/v1/api-keys");
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data;
    },
    enabled: tenantId.length > 0,
    ...options,
  });
}

export function useCreateApiKey(tenantId: string) {
  const qc = useQueryClient();
  return useMutation<CreateApiKeyResponse, ApiError, CreateApiKeyRequest>({
    mutationFn: async (body) => {
      const { data, error, response } = await apiClient.POST(
        "/v1/api-keys",
        { body },
      );
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data;
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: apiKeysQueryKey(tenantId) });
    },
  });
}

export function useRevokeApiKey(tenantId: string) {
  const qc = useQueryClient();
  return useMutation<void, ApiError, string>({
    mutationFn: async (id) => {
      const { error, response } = await apiClient.DELETE(
        "/v1/api-keys/{id}",
        { params: { path: { id } } },
      );
      if (error) {
        throw toApiError(response.status, error, response);
      }
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: apiKeysQueryKey(tenantId) });
    },
  });
}
