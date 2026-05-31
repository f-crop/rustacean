import { useQuery, type UseQueryOptions } from "@tanstack/react-query";
import { apiClient, toApiError, type ApiError } from "../client";
import type { components } from "../generated/schema";

type AuditEventItem = components["schemas"]["AuditEventItem"];
type AuditListResponse = components["schemas"]["AuditListResponse"];

export type { AuditEventItem, AuditListResponse };

export interface AuditEventsParams {
  readonly from?: string;
  readonly to?: string;
  readonly action?: string;
  readonly limit?: number;
}

export const auditEventsQueryKey = (
  tenantId: string,
  params?: AuditEventsParams,
) => ["tenants", tenantId, "audit", params] as const;

export function useAuditEvents(
  tenantId: string,
  params?: AuditEventsParams,
  options?: Omit<
    UseQueryOptions<AuditListResponse, ApiError>,
    "queryKey" | "queryFn"
  >,
) {
  return useQuery<AuditListResponse, ApiError>({
    queryKey: auditEventsQueryKey(tenantId, params),
    queryFn: async () => {
      const { data, error, response } = await apiClient.GET("/v1/audit", {
        params: {
          query: {
            limit: params?.limit ?? 100,
            from: params?.from ?? null,
            to: params?.to ?? null,
            action: params?.action ?? null,
          },
        },
      });
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data;
    },
    enabled: tenantId.length > 0,
    ...options,
  });
}
