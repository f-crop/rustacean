import { useMutation, useQueryClient } from "@tanstack/react-query";
import { apiClient, toApiError, type ApiError } from "../client";
import type { components } from "../generated/schema";
import { meQueryKey } from "./useMe";

type SignupRequest = components["schemas"]["SignupRequest"];
type SignupResponse = components["schemas"]["SignupResponse"];

export function useSignup() {
  const qc = useQueryClient();
  return useMutation<SignupResponse, ApiError, SignupRequest>({
    mutationFn: async (body) => {
      const { data, error, response } = await apiClient.POST(
        "/v1/auth/signup",
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
