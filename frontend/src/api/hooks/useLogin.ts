import { useMutation, useQueryClient } from "@tanstack/react-query";
import { apiClient, toApiError, type ApiError } from "../client";
import type { components } from "../generated/schema";
import { meQueryKey } from "./useMe";

type LoginRequest = components["schemas"]["LoginRequest"];
type LoginResponse = components["schemas"]["LoginResponse"];

export function useLogin() {
  const qc = useQueryClient();
  return useMutation<LoginResponse, ApiError, LoginRequest>({
    mutationFn: async (body) => {
      const { data, error, response } = await apiClient.POST(
        "/v1/auth/login",
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
