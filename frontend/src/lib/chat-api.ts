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
  /** UUID v4 minted per-turn; ties the optimistic bubble to the SSE stream. */
  turn_id: string;
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
  /** v2: UUID of the turn this event belongs to. Absent for legacy v1 events. */
  turn_id?: string;
  /** v2: user message id that opened this turn; null on user_input frames. */
  parent_user_id?: string | null;
  /** v2: protocol version (2 when turn_id is present). */
  protocol_version?: number;
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
  /** v2: UUID of the turn this message belongs to. Absent for legacy rows (pre-022). */
  turn_id?: string;
  /** v2: for assistant rows — the user message id that triggered this turn. */
  parent_user_id?: string;
}

export interface ListMessagesResponse {
  messages: ChatMessage[];
  has_more: boolean;
}

