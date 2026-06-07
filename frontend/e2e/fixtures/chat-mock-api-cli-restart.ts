// Fixtures for the CLI-restart replay dedup regression spec.
// The CLI replays the full conversation history after user_input("8+8"), which
// without the fix causes historical assistant responses ("4", "14") to appear
// as extra bubbles after the current user message.

import { CHAT_SESSION_ID } from "./chat-mock-api";

// ─── 2-turn UAT scenario (RUSAA-1942) ──────────────────────────────────────
// Session: 2 turns of math (4+4=8, 8+8=16).
// CLI restarts AFTER user_input("8+8") was processed; the SSE stream reconnects
// mid-turn-2 and has NO user_input event — only replayed + streaming assistant tokens.
// Without the fix, the replayed "8" appears as a duplicate after "what is 8+8".

// DB state: turn-1 complete, turn-2 user stored but asst not yet flushed.
export const LIST_MESSAGES_TWO_TURNS_MATH_IN_PROGRESS = {
  messages: [
    { id: "r42-u1", seq: 1, role: "user", body: "what is 4+4", created_at: "2026-06-07T00:00:00Z" },
    { id: "r42-a1", seq: 2, role: "assistant", body: "8", created_at: "2026-06-07T00:00:01Z" },
    { id: "r42-u2", seq: 3, role: "user", body: "what is 8+8", created_at: "2026-06-07T00:00:02Z" },
  ],
  has_more: false,
};

// SSE: no user_input event (CLI restarted mid-turn-2 after user_input processed).
// text("8") is the replayed turn-1 assistant; turn_complete flushes it.
// text("16") is the actual turn-2 response, still streaming (no final turn_complete).
export const TWO_TURN_NO_USER_INPUT_REPLAY_SSE = [
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 5,
    payload: { type: "text", text: "8" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "turn_complete",
    sequence: 6,
    payload: { type: "turn_complete", stop_reason: "end_turn" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 7,
    payload: { type: "text", text: "16" },
  })}`,
  "",
  "",
].join("\n");

// 3-turn session history (turns 1+2 complete, turn 3 in progress).
export const LIST_MESSAGES_THREE_TURNS_IN_PROGRESS = {
  messages: [
    { id: "r57-u1", seq: 1, role: "user", body: "what is 2+2", created_at: "2026-06-07T00:00:00Z" },
    { id: "r57-a1", seq: 2, role: "assistant", body: "4", created_at: "2026-06-07T00:00:01Z" },
    { id: "r57-u2", seq: 3, role: "user", body: "what is 7+7", created_at: "2026-06-07T00:00:02Z" },
    { id: "r57-a2", seq: 4, role: "assistant", body: "14", created_at: "2026-06-07T00:00:03Z" },
    { id: "r57-u3", seq: 5, role: "user", body: "what is 8+8", created_at: "2026-06-07T00:00:04Z" },
  ],
  has_more: false,
};

// SSE for the CLI-restart scenario. After user_input("8+8") the CLI replays
// the full history: text("4")+tc (turn-1 replay) + text("14")+tc (turn-2 replay)
// + text("16") (actual turn-3 response, still streaming).
// With dedupeAssistantsPerSegment, only "16" renders after "what is 8+8".
export const THREE_TURN_REPLAY_SSE = [
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "user_input",
    sequence: 1,
    payload: { type: "user_input", text: "what is 2+2" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 2,
    payload: { type: "text", text: "4" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "turn_complete",
    sequence: 3,
    payload: { type: "turn_complete", stop_reason: "end_turn" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "user_input",
    sequence: 4,
    payload: { type: "user_input", text: "what is 7+7" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 5,
    payload: { type: "text", text: "14" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "turn_complete",
    sequence: 6,
    payload: { type: "turn_complete", stop_reason: "end_turn" },
  })}`,
  "",
  // Turn 3: user_input followed by REPLAYED historical responses (turns 1+2),
  // then the actual turn-3 response (still streaming, no turn_complete).
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "user_input",
    sequence: 7,
    payload: { type: "user_input", text: "what is 8+8" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 8,
    payload: { type: "text", text: "4" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "turn_complete",
    sequence: 9,
    payload: { type: "turn_complete", stop_reason: "end_turn" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 10,
    payload: { type: "text", text: "14" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "turn_complete",
    sequence: 11,
    payload: { type: "turn_complete", stop_reason: "end_turn" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 12,
    payload: { type: "text", text: "16" },
  })}`,
  "",
  "",
].join("\n");
