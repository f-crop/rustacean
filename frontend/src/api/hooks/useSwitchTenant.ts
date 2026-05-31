import { useMutation, useQueryClient } from "@tanstack/react-query";
import { apiClient, toApiError, type ApiError } from "../client";
import type { components } from "../generated/schema";
import { meQueryKey } from "./useMe";

type SwitchTenantRequest = components["schemas"]["SwitchTenantRequest"];
type SwitchTenantResponse = components["schemas"]["SwitchTenantResponse"];

export function useSwitchTenant() {
  const qc = useQueryClient();
  return useMutation<SwitchTenantResponse, ApiError, SwitchTenantRequest>({
    mutationFn: async (body) => {
      const { data, error, response } = await apiClient.POST(
        "/v1/me/switch-tenant",
        { body },
      );
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data;
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: meQueryKey });
    },
  });
}
