// Chat session API hooks.
// Uses chatApiClient (locally-typed wrapper) because /v1/chat/* paths are not yet
// in the generated openapi.json (S3 is a peer stream). Once S3 ships and
// `npm run gen:api` regenerates the schema, migrate to the main apiClient.

import { z } from "zod";
import { useMutation, useQuery, useQueryClient, type UseQueryOptions } from "@tanstack/react-query";
import { toApiError, type ApiError } from "../client";
import { chatApiClient } from "../chat-client";
import type {
  ListChatSessionsResponse,
  ListMessagesResponse,
  CreateChatSessionRequest,
  CreateChatSessionResponse,
  SendMessageRequest,
  SendMessageResponse,
  ChatSession,
} from "@/lib/chat-api";

// Zod schema that accepts both `id` (current) and `session_id` (server legacy shape) and
// normalises either to `id`. Any session row missing both is dropped rather than crashing.
const chatSessionSchema = z
  .object({
    id: z.string().optional(),
    session_id: z.string().optional(),
    tenant_id: z.string().default(""),
    user_id: z.string().nullable().default(null),
    runtime: z.enum(["claude_code", "opencode", "pi"]).default("claude_code"),
    status: z.enum(["active", "ended", "failed"]).default("active"),
    trace_id: z.string().default(""),
    created_at: z.string().default(""),
    last_activity_at: z.string().default(""),
    ended_at: z.string().nullable().default(null),
  })
  .transform((raw) => {
    const id = raw.id ?? raw.session_id ?? "";
    return {
      id,
      tenant_id: raw.tenant_id,
      user_id: raw.user_id,
      runtime: raw.runtime,
      status: raw.status,
      trace_id: raw.trace_id,
      created_at: raw.created_at,
      last_activity_at: raw.last_activity_at,
      ended_at: raw.ended_at,
    };
  });

function parseSessionsResponse(raw: unknown): ListChatSessionsResponse {
  const envelope = z.object({ sessions: z.array(z.unknown()).default([]) }).safeParse(raw);
  if (!envelope.success) return { sessions: [] };
  const sessions: ChatSession[] = [];
  for (const item of envelope.data.sessions) {
    const result = chatSessionSchema.safeParse(item);
    if (result.success && result.data.id.length > 0) {
      sessions.push(result.data as ChatSession);
    }
  }
  return { sessions };
}

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
      return parseSessionsResponse(data);
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

export const chatMessagesQueryKey = (sessionId: string) =>
  ["chat-sessions", sessionId, "messages"] as const;

export function useChatMessages(sessionId: string | null) {
  return useQuery<ListMessagesResponse, ApiError>({
    queryKey: chatMessagesQueryKey(sessionId ?? ""),
    queryFn: async () => {
      const { data, error, response } = await chatApiClient.listMessages(sessionId!);
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data;
    },
    enabled: sessionId !== null,
    staleTime: 30_000,
  });
}

export type SendMessageVariables = SendMessageRequest & { sessionId: string };

export function useSendChatMessage() {
  const qc = useQueryClient();
  return useMutation<SendMessageResponse, ApiError, SendMessageVariables>({
    mutationFn: async ({ sessionId, ...body }) => {
      const { data, error, response } = await chatApiClient.sendMessage(sessionId, body);
      if (error || !data) {
        throw toApiError(response.status, error, response);
      }
      return data;
    },
    onSuccess: (_, { sessionId }) => {
      // Refresh historical messages so that the sent message is covered by
      // coveredTexts in ChatPage's transcript memo. Without this, if the SSE
      // relay drops the user_input echo, the pending bubble stays uncovered and
      // gets appended after all assistant content, causing the classic ordering
      // inversion (assistant1, assistant2, user1, user2).
      void qc.invalidateQueries({ queryKey: chatMessagesQueryKey(sessionId) });
    },
  });
}

export type {
  ChatSession,
  ListChatSessionsResponse,
  ListMessagesResponse,
  CreateChatSessionRequest,
  CreateChatSessionResponse,
  SendMessageRequest,
  SendMessageResponse,
};
