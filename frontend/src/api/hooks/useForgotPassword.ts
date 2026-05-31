import { useMutation } from "@tanstack/react-query";
import { apiClient, toApiError, type ApiError } from "../client";
import type { components } from "../generated/schema";

type ForgotPasswordRequest = components["schemas"]["ForgotPasswordRequest"];

export function useForgotPassword() {
  return useMutation<void, ApiError, ForgotPasswordRequest>({
    mutationFn: async (body) => {
      const result = await apiClient.POST("/v1/auth/forgot-password", { body });
      const status = result.response.status;
      if (result.error || !result.response.ok) {
        throw toApiError(status, result.error ?? null, result.response);
      }
    },
  });
}
