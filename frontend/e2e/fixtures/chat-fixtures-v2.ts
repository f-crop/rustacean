import { CHAT_SESSION_ID, TURN_ID_A, TURN_ID_B } from "./chat-fixtures";

function v2sseEvent(
  sessionId: string,
  payload: Record<string, unknown>,
  sequence: number,
  turnId?: string,
): string {
  const envelope: Record<string, unknown> = {
    session_id: sessionId,
    event_type: String(payload.type),
    sequence,
    payload,
    ...(turnId ? { turn_id: turnId, protocol_version: 2 } : {}),
  };
  return [
    "event: session.event",
    `data: ${JSON.stringify(envelope)}`,
    "",
  ].join("\n");
}

// Single v2 turn: user sends "what is ownership?", assistant streams then completes.
export const V2_SINGLE_TURN_STREAMING_SSE = [
  v2sseEvent(CHAT_SESSION_ID, { type: "user_input", text: "what is ownership?" }, 1, TURN_ID_A),
  v2sseEvent(CHAT_SESSION_ID, { type: "text", text: "Ownership is Rust's memory safety model." }, 2, TURN_ID_A),
  "",
].join("\n");

// Same single turn but turn_complete arrives — isStreaming should go false.
export const V2_SINGLE_TURN_COMPLETE_SSE = [
  v2sseEvent(CHAT_SESSION_ID, { type: "user_input", text: "what is ownership?" }, 1, TURN_ID_A),
  v2sseEvent(CHAT_SESSION_ID, { type: "text", text: "Ownership is Rust's memory safety model." }, 2, TURN_ID_A),
  v2sseEvent(CHAT_SESSION_ID, { type: "turn_complete", stop_reason: "end_turn" }, 3, TURN_ID_A),
  "",
].join("\n");

// Two v2 turns: first complete, second streaming.
export const V2_TWO_TURNS_SECOND_STREAMING_SSE = [
  v2sseEvent(CHAT_SESSION_ID, { type: "user_input", text: "q1" }, 1, TURN_ID_A),
  v2sseEvent(CHAT_SESSION_ID, { type: "text", text: "Reply 1" }, 2, TURN_ID_A),
  v2sseEvent(CHAT_SESSION_ID, { type: "turn_complete", stop_reason: "end_turn" }, 3, TURN_ID_A),
  v2sseEvent(CHAT_SESSION_ID, { type: "user_input", text: "q2" }, 4, TURN_ID_B),
  v2sseEvent(CHAT_SESSION_ID, { type: "text", text: "Reply 2..." }, 5, TURN_ID_B),
  "",
].join("\n");

// CLI restart without user_input: TURN_A replayed (completed), TURN_B streaming.
export const V2_CLI_RESTART_STREAMING_SSE = [
  v2sseEvent(CHAT_SESSION_ID, { type: "text", text: "Reply 1" }, 2, TURN_ID_A),
  v2sseEvent(CHAT_SESSION_ID, { type: "turn_complete", stop_reason: "end_turn" }, 3, TURN_ID_A),
  v2sseEvent(CHAT_SESSION_ID, { type: "text", text: "Reply 2..." }, 5, TURN_ID_B),
  "",
].join("\n");

// History: two complete v2 turns.
export const LIST_MESSAGES_V2_TWO_TURNS = {
  messages: [
    {
      id: "v2-u1",
      seq: 1,
      role: "user",
      body: "q1",
      created_at: "2026-06-10T00:00:00Z",
      turn_id: TURN_ID_A,
    },
    {
      id: "v2-a1",
      seq: 2,
      role: "assistant",
      body: JSON.stringify([{ type: "text", text: "Reply 1" }]),
      created_at: "2026-06-10T00:00:01Z",
      turn_id: TURN_ID_A,
    },
    {
      id: "v2-u2",
      seq: 3,
      role: "user",
      body: "q2",
      created_at: "2026-06-10T00:00:02Z",
      turn_id: TURN_ID_B,
    },
  ],
  has_more: false,
};

// History: first complete v2 turn only (assistant not yet in DB for TURN_B).
export const LIST_MESSAGES_V2_TURN_A_COMPLETE = {
  messages: [
    {
      id: "v2-u1",
      seq: 1,
      role: "user",
      body: "q1",
      created_at: "2026-06-10T00:00:00Z",
      turn_id: TURN_ID_A,
    },
    {
      id: "v2-a1",
      seq: 2,
      role: "assistant",
      body: JSON.stringify([{ type: "text", text: "Reply 1" }]),
      created_at: "2026-06-10T00:00:01Z",
      turn_id: TURN_ID_A,
    },
  ],
  has_more: false,
};
