import { useMutation } from "@tanstack/react-query";
import { apiClient, toApiError, type ApiError } from "../client";
import type { components } from "../generated/schema";

type ResendVerificationRequest = components["schemas"]["ResendVerificationRequest"];

export function useResendVerification() {
  return useMutation<void, ApiError, ResendVerificationRequest>({
    mutationFn: async (body) => {
      const { error, response } = await apiClient.POST(
        "/v1/auth/resend-verification",
        { body },
      );
      if (error) {
        throw toApiError(response.status, error, response);
      }
    },
  });
}
