// Chat API types derived from ADR-013 contract.
// TODO: once S3 ships and `npm run gen:api` regenerates the schema,
// replace these hand-written types with imports from @/api/generated/schema.

export type ChatRuntime = "claude_code" | "opencode" | "pi";
export type ChatSessionStatus = "active" | "ended" | "failed";
export type ChatMessageRole = "user" | "assistant" | "system" | "tool";

export interface ChatSession {
  id: string;
  tenant_id: string;
  user_id: string | null;
  runtime: ChatRuntime;
  status: ChatSessionStatus;
  trace_id: string;
  created_at: string;
  last_activity_at: string;
  ended_at: string | null;
}

export interface ListChatSessionsResponse {
  sessions: ChatSession[];
}

export interface CreateChatSessionRequest {
  runtime: ChatRuntime;
}

export interface CreateChatSessionResponse {
  session_id: string;
}

export interface SendMessageRequest {
  content: string;
}

export interface SendMessageResponse {
  message_id: string;
}

// SSE event envelope — reuses the same agent-runner relay format (ADR-013 §7).
// The chat event relay publishes over the same infrastructure as agent sessions,
// so the envelope shape is identical.
export type ChatRuntimePayload =
  | { type: "text"; text: string }
  | { type: "thinking"; thinking: string }
  | { type: "tool_use"; id: string; name: string; input: unknown }
  | { type: "tool_result"; tool_use_id: string; content: unknown; is_error: boolean }
  | { type: "error"; message: string; code?: string }
  | { type: "user_input"; text: string }
  | { type: "turn_complete"; stop_reason: string };

export interface ChatSessionEventEnvelope {
  session_id: string;
  event_type: string;
  sequence: number;
  payload: ChatRuntimePayload;
}

export interface ChatSessionErrorEnvelope {
  error: string;
  status: string;
  message: string;
}

export interface ChatMessage {
  id: string;
  seq: number;
  role: ChatMessageRole;
  body: string;
  created_at: string;
}

export interface ListMessagesResponse {
  messages: ChatMessage[];
  has_more: boolean;
}

