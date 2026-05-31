import { useMutation } from "@tanstack/react-query";
import { apiClient, toApiError, type ApiError } from "../client";
import type { components } from "../generated/schema";

type InstallUrlResponse = components["schemas"]["InstallUrlResponse"];

export function useGithubInstallUrl() {
  return useMutation<InstallUrlResponse, ApiError, void>({
    mutationFn: async () => {
      const { data, error, response } = await apiClient.GET("/v1/github/install-url");
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data;
    },
  });
}
