// Chat session API hooks.
// Uses chatApiClient (locally-typed wrapper) because /v1/chat/* paths are not yet
// in the generated openapi.json (S3 is a peer stream). Once S3 ships and
// `npm run gen:api` regenerates the schema, migrate to the main apiClient.

import { useMutation, useQuery, useQueryClient, type UseQueryOptions } from "@tanstack/react-query";
import { toApiError, type ApiError } from "../client";
import { chatApiClient } from "../chat-client";
import type {
  ListChatSessionsResponse,
  CreateChatSessionRequest,
  CreateChatSessionResponse,
  SendMessageRequest,
  SendMessageResponse,
  ChatSession,
} from "@/lib/chat-api";

export const chatSessionsQueryKey = (tenantId: string) =>
  ["tenants", tenantId, "chat-sessions"] as const;

export function useChatSessions(
  tenantId: string,
  options?: Omit<
    UseQueryOptions<ListChatSessionsResponse, ApiError>,
    "queryKey" | "queryFn"
  >,
) {
  return useQuery<ListChatSessionsResponse, ApiError>({
    queryKey: chatSessionsQueryKey(tenantId),
    queryFn: async () => {
      const { data, error, response } = await chatApiClient.listSessions();
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data;
    },
    enabled: tenantId.length > 0,
    staleTime: 15_000,
    refetchInterval: 10_000,
    ...options,
  });
}

export function useCreateChatSession(tenantId: string) {
  const qc = useQueryClient();
  return useMutation<CreateChatSessionResponse, ApiError, CreateChatSessionRequest>({
    mutationFn: async (body) => {
      const { data, error, response } = await chatApiClient.createSession(body);
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data;
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: chatSessionsQueryKey(tenantId) });
    },
  });
}

export type SendMessageVariables = SendMessageRequest & { sessionId: string };

export function useSendChatMessage() {
  return useMutation<SendMessageResponse, ApiError, SendMessageVariables>({
    mutationFn: async ({ sessionId, ...body }) => {
      const { data, error, response } = await chatApiClient.sendMessage(sessionId, body);
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data;
    },
  });
}

export type {
  ChatSession,
  ListChatSessionsResponse,
  CreateChatSessionRequest,
  CreateChatSessionResponse,
  SendMessageRequest,
  SendMessageResponse,
};
