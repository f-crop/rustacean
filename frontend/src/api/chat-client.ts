// Typed wrapper for /v1/chat/* API calls.
// The chat paths are not yet in the generated openapi.json (S3 is a peer stream);
// this module provides a typed surface until `npm run gen:api` is re-run after S3 ships.
// At that point, delete this file and migrate to the main apiClient in ./client.ts.

import createClient from "openapi-fetch";
import type {
  CreateChatSessionRequest,
  CreateChatSessionResponse,
  ListChatSessionsResponse,
  ListMessagesResponse,
  SendMessageRequest,
  SendMessageResponse,
} from "@/lib/chat-api";

function resolveBaseUrl(): string {
  const fromEnv = import.meta.env.VITE_API_BASE_URL as string | undefined;
  return fromEnv?.replace(/\/$/, "") ?? "";
}

// We use `createClient` (not raw `fetch`) so the middleware stack (trace IDs,
// credentials) is consistent with the main apiClient. The `as unknown as` cast
// is unavoidable here: the /v1/chat/* path types are not in the generated
// OpenAPI schema yet (S3 peer stream), so we define the contract locally and
// cast once at the boundary rather than carrying `any` through the hooks.
const _rawClient = createClient({
  baseUrl: resolveBaseUrl(),
  credentials: "include",
  headers: { "Content-Type": "application/json" },
});

type FetchResponse<T> = {
  data: T | undefined;
  error: unknown;
  response: Response;
};

export const chatApiClient = {
  listSessions: (): Promise<FetchResponse<ListChatSessionsResponse>> =>
    (_rawClient.GET as (path: string) => Promise<FetchResponse<ListChatSessionsResponse>>)(
      "/v1/chat/sessions",
    ),

  createSession: (
    body: CreateChatSessionRequest,
  ): Promise<FetchResponse<CreateChatSessionResponse>> =>
    (
      _rawClient.POST as (
        path: string,
        opts: { body: CreateChatSessionRequest },
      ) => Promise<FetchResponse<CreateChatSessionResponse>>
    )("/v1/chat/sessions", { body }),

  sendMessage: (
    sessionId: string,
    body: SendMessageRequest,
  ): Promise<FetchResponse<SendMessageResponse>> =>
    (
      _rawClient.POST as (
        path: string,
        opts: { params: { path: { id: string } }; body: SendMessageRequest },
      ) => Promise<FetchResponse<SendMessageResponse>>
    )("/v1/chat/sessions/{id}/messages", { params: { path: { id: sessionId } }, body }),

  listMessages: (
    sessionId: string,
  ): Promise<FetchResponse<ListMessagesResponse>> =>
    (
      _rawClient.GET as (
        path: string,
        opts: { params: { path: { id: string } } },
      ) => Promise<FetchResponse<ListMessagesResponse>>
    )("/v1/chat/sessions/{id}/messages", { params: { path: { id: sessionId } } }),
};
