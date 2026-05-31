import { useMutation } from "@tanstack/react-query";
import { apiClient, toApiError, type ApiError } from "../client";
import type { components } from "../generated/schema";

type ResetPasswordRequest = components["schemas"]["ResetPasswordRequest"];

export function useResetPassword() {
  return useMutation<void, ApiError, ResetPasswordRequest>({
    mutationFn: async (body) => {
      const { error, response } = await apiClient.POST(
        "/v1/auth/reset-password",
        { body },
      );
      if (error) {
        throw toApiError(response.status, error, response);
      }
    },
  });
}
