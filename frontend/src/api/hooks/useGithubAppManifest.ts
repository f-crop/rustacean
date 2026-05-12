import { useMutation } from "@tanstack/react-query";
import { apiClient, toApiError, type ApiError } from "../client";
import type { components } from "../generated/schema";

type AppManifestRequest = components["schemas"]["AppManifestRequest"];
type AppManifestResponse = components["schemas"]["AppManifestResponse"];

export function useGithubAppManifest() {
  return useMutation<AppManifestResponse, ApiError, AppManifestRequest>({
    mutationFn: async (body) => {
      const { data, error, response } = await apiClient.POST(
        "/v1/admin/github/app-manifest",
        { body },
      );
      if (error || !data) {
        throw toApiError(response.status, error);
      }
      return data;
    },
  });
}
