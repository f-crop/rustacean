import { useMutation, useQueryClient, type UseMutationOptions } from "@tanstack/react-query";
import { apiClient, toApiError, type ApiError } from "../client";
import type { components } from "../generated/schema";
import { agentSessionsQueryKey } from "./useAgentSessions";

type CreateSessionRequest = components["schemas"]["CreateSessionRequest"];

export function useCreateSession(
  options?: Omit<
    UseMutationOptions<void, ApiError, CreateSessionRequest>,
    "mutationFn"
  >,
) {
  const qc = useQueryClient();
  return useMutation<void, ApiError, CreateSessionRequest>({
    mutationFn: async (body) => {
      const { error, response } = await apiClient.POST(
        "/v1/agents/sessions",
        { body },
      );
      if (error) {
        throw toApiError(response.status, error);
      }
    },
    onSuccess: (...args) => {
      void qc.invalidateQueries({ queryKey: agentSessionsQueryKey });
      options?.onSuccess?.(...args);
    },
    ...options,
  });
}
