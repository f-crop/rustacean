import {
  useMutation,
  useQuery,
  useQueryClient,
  type UseQueryOptions,
} from "@tanstack/react-query";
import { apiClient, toApiError, type ApiError } from "../client";
import type { components } from "../generated/schema";
import { meQueryKey } from "./useMe";

type ListMembersResponse = components["schemas"]["ListMembersResponse"];
type InviteMemberRequest = components["schemas"]["InviteMemberRequest"];
type InviteMemberResponse = components["schemas"]["InviteMemberResponse"];
type UpdateRoleRequest = components["schemas"]["UpdateRoleRequest"];
type UpdateRoleResponse = components["schemas"]["UpdateRoleResponse"];
type TransferOwnershipRequest =
  components["schemas"]["TransferOwnershipRequest"];

export const tenantMembersQueryKey = (tenantId: string) =>
  ["tenants", tenantId, "members"] as const;

export function useTenantMembers(
  tenantId: string,
  options?: Omit<
    UseQueryOptions<ListMembersResponse, ApiError>,
    "queryKey" | "queryFn"
  >,
) {
  return useQuery<ListMembersResponse, ApiError>({
    queryKey: tenantMembersQueryKey(tenantId),
    queryFn: async () => {
      const { data, error, response } = await apiClient.GET(
        "/v1/tenants/{id}/members",
        { params: { path: { id: tenantId } } },
      );
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data;
    },
    enabled: tenantId.length > 0,
    ...options,
  });
}

export function useInviteMember(tenantId: string) {
  const qc = useQueryClient();
  return useMutation<InviteMemberResponse, ApiError, InviteMemberRequest>({
    mutationFn: async (body) => {
      const { data, error, response } = await apiClient.POST(
        "/v1/tenants/{id}/members",
        { params: { path: { id: tenantId } }, body },
      );
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data;
    },
    onSettled: () => {
      void qc.invalidateQueries({ queryKey: tenantMembersQueryKey(tenantId) });
    },
  });
}

export function useUpdateMemberRole(tenantId: string) {
  const qc = useQueryClient();
  return useMutation<
    UpdateRoleResponse,
    ApiError,
    { uid: string; body: UpdateRoleRequest }
  >({
    mutationFn: async ({ uid, body }) => {
      const { data, error, response } = await apiClient.PUT(
        "/v1/tenants/{id}/members/{uid}/role",
        { params: { path: { id: tenantId, uid } }, body },
      );
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data;
    },
    onSettled: () => {
      void qc.invalidateQueries({ queryKey: tenantMembersQueryKey(tenantId) });
    },
  });
}

export function useRemoveMember(tenantId: string) {
  const qc = useQueryClient();
  return useMutation<void, ApiError, string>({
    mutationFn: async (uid) => {
      const { error, response } = await apiClient.DELETE(
        "/v1/tenants/{id}/members/{uid}",
        { params: { path: { id: tenantId, uid } } },
      );
      if (error) {
        throw toApiError(response.status, error, response);
      }
    },
    onSettled: () => {
      void qc.invalidateQueries({ queryKey: tenantMembersQueryKey(tenantId) });
    },
  });
}

export function useTransferOwnership(tenantId: string) {
  const qc = useQueryClient();
  return useMutation<void, ApiError, TransferOwnershipRequest>({
    mutationFn: async (body) => {
      const { error, response } = await apiClient.POST(
        "/v1/tenants/{id}/transfer-ownership",
        { params: { path: { id: tenantId } }, body },
      );
      if (error) {
        throw toApiError(response.status, error, response);
      }
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: meQueryKey });
      void qc.invalidateQueries({ queryKey: tenantMembersQueryKey(tenantId) });
    },
  });
}
