import { useMutation, useQueryClient } from "@tanstack/react-query";
import { apiClient, toApiError, type ApiError } from "../client";
import type { components } from "../generated/schema";
import { meQueryKey } from "./useMe";

type VerifyEmailRequest = components["schemas"]["VerifyEmailRequest"];

export function useVerifyEmail() {
  const qc = useQueryClient();
  return useMutation<void, ApiError, VerifyEmailRequest>({
    mutationFn: async (body) => {
      const { error, response } = await apiClient.POST(
        "/v1/auth/verify-email",
        { body },
      );
      if (error) {
        throw toApiError(response.status, error, response);
      }
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: meQueryKey });
    },
  });
}
