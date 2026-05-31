import { useMutation, useQueryClient } from "@tanstack/react-query";
import { apiClient, toApiError, type ApiError } from "../client";

export function useLogout() {
  const qc = useQueryClient();
  return useMutation<void, ApiError, void>({
    mutationFn: async () => {
      const { error, response } = await apiClient.POST("/v1/auth/logout");
      if (error) {
        throw toApiError(response.status, error, response);
      }
    },
    onSuccess: () => {
      qc.clear();
    },
  });
}
