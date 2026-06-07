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

// ─── 4-turn UAT scenario (RUSAA-1944 / R24) ────────────────────────────────
// Session: 3 completed turns (2+2=4, 8+8=16, 16+16=32) + new turn-4 in flight.
// The CLI receives user_input("what is 100+100") and replays ALL prior responses
// before generating the real answer. Without the fix, dedupeAssistantsPerSegment
// keeps replay3("32") as turn-4's assistant bubble.

// DB state: 3 turns complete, turn-4 user stored, no asst yet.
export const LIST_MESSAGES_FOUR_TURNS_IN_PROGRESS = {
  messages: [
    { id: "r44-u1", seq: 1, role: "user", body: "what is 2+2", created_at: "2026-06-07T00:00:00Z" },
    { id: "r44-a1", seq: 2, role: "assistant", body: "4", created_at: "2026-06-07T00:00:01Z" },
    { id: "r44-u2", seq: 3, role: "user", body: "what is 8+8", created_at: "2026-06-07T00:00:03Z" },
    { id: "r44-a2", seq: 4, role: "assistant", body: "16", created_at: "2026-06-07T00:00:04Z" },
    { id: "r44-u3", seq: 5, role: "user", body: "what is 16+16", created_at: "2026-06-07T00:00:05Z" },
    { id: "r44-a3", seq: 6, role: "assistant", body: "32", created_at: "2026-06-07T00:00:06Z" },
    { id: "r44-u4", seq: 7, role: "user", body: "what is 100+100", created_at: "2026-06-07T00:00:07Z" },
  ],
  has_more: false,
};

// SSE: 3 prior turns complete, then user_input for turn-4 followed by CLI replaying
// all 3 prior turns before the real answer arrives. No streaming "200" yet.
// Bug: dedupeAssistantsPerSegment kept replay3("32") in turn-4's slot.
export const FOUR_TURN_CLI_REPLAY_SSE_NO_INPROGRESS = [
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "user_input", sequence: 1, payload: { type: "user_input", text: "what is 2+2" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "text", sequence: 2, payload: { type: "text", text: "4" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 3, payload: { type: "turn_complete", stop_reason: "end_turn" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "user_input", sequence: 4, payload: { type: "user_input", text: "what is 8+8" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "text", sequence: 5, payload: { type: "text", text: "16" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 6, payload: { type: "turn_complete", stop_reason: "end_turn" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "user_input", sequence: 7, payload: { type: "user_input", text: "what is 16+16" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "text", sequence: 8, payload: { type: "text", text: "32" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 9, payload: { type: "turn_complete", stop_reason: "end_turn" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "user_input", sequence: 10, payload: { type: "user_input", text: "what is 100+100" } })}`,
  "",
  // CLI replays turns 1-3 before the real answer arrives.
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "text", sequence: 11, payload: { type: "text", text: "4" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 12, payload: { type: "turn_complete", stop_reason: "end_turn" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "text", sequence: 13, payload: { type: "text", text: "16" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 14, payload: { type: "turn_complete", stop_reason: "end_turn" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "text", sequence: 15, payload: { type: "text", text: "32" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 16, payload: { type: "turn_complete", stop_reason: "end_turn" } })}`,
  "",
  "",
].join("\n");

// Same scenario but with the actual "200" arriving (still streaming, no turn_complete).
// Verifies that the in-progress path shows "200" for turn-4, not any replay text.
export const FOUR_TURN_CLI_REPLAY_SSE_WITH_INPROGRESS = [
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "user_input", sequence: 1, payload: { type: "user_input", text: "what is 2+2" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "text", sequence: 2, payload: { type: "text", text: "4" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 3, payload: { type: "turn_complete", stop_reason: "end_turn" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "user_input", sequence: 4, payload: { type: "user_input", text: "what is 8+8" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "text", sequence: 5, payload: { type: "text", text: "16" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 6, payload: { type: "turn_complete", stop_reason: "end_turn" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "user_input", sequence: 7, payload: { type: "user_input", text: "what is 16+16" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "text", sequence: 8, payload: { type: "text", text: "32" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 9, payload: { type: "turn_complete", stop_reason: "end_turn" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "user_input", sequence: 10, payload: { type: "user_input", text: "what is 100+100" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "text", sequence: 11, payload: { type: "text", text: "4" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 12, payload: { type: "turn_complete", stop_reason: "end_turn" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "text", sequence: 13, payload: { type: "text", text: "16" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 14, payload: { type: "turn_complete", stop_reason: "end_turn" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "text", sequence: 15, payload: { type: "text", text: "32" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 16, payload: { type: "turn_complete", stop_reason: "end_turn" } })}`,
  "",
  // Actual turn-4 answer arrives (still streaming — no turn_complete).
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "text", sequence: 17, payload: { type: "text", text: "200" } })}`,
  "",
  "",
].join("\n");

// ─── 3-turn UAT scenario (RUSAA-1946 / R26 fixup) ──────────────────────────
// Session: 2 completed turns (2+2=4, 4+4=8) + turn-3 (8+8) whose SSE stream
// just finished.  The SSE has NO user_input events and replays turns 1+2 before
// emitting the fresh turn-3 completion.
//
// DB seq values: ass1.seq=2, ass2.seq=4 → histAssistantSeqs = {2, 4}.
// SSE replays use the same sequence numbers (startSeq=2, startSeq=4) so the
// histAssistantSeqs filter drops them, leaving only the fresh ass3 (startSeq=6).

// DB state: turns 1+2 fully persisted, turn-3 user stored but assistant not yet.
export const LIST_MESSAGES_R26_NO_ASS3 = {
  messages: [
    { id: "r46-u1", seq: 1, role: "user", body: "what is 2+2", created_at: "2026-06-07T00:00:00Z" },
    { id: "r46-a1", seq: 2, role: "assistant", body: "4", created_at: "2026-06-07T00:00:01Z" },
    { id: "r46-u2", seq: 3, role: "user", body: "what is 4+4", created_at: "2026-06-07T00:00:02Z" },
    { id: "r46-a2", seq: 4, role: "assistant", body: "8", created_at: "2026-06-07T00:00:03Z" },
    { id: "r46-u3", seq: 5, role: "user", body: "what is 8+8", created_at: "2026-06-07T00:00:04Z" },
    // turn-3 assistant NOT yet persisted
  ],
  has_more: false,
};

// DB state: turns 1+2+3 fully persisted, turn-4 user stored but assistant not yet.
// Used for the R24-!firstLiveUser regression guard.
export const LIST_MESSAGES_R26_THREE_FULL_PLUS_USER4 = {
  messages: [
    { id: "r46-r24-u1", seq: 1, role: "user", body: "what is 2+2", created_at: "2026-06-07T00:00:00Z" },
    { id: "r46-r24-a1", seq: 2, role: "assistant", body: "4", created_at: "2026-06-07T00:00:01Z" },
    { id: "r46-r24-u2", seq: 3, role: "user", body: "what is 4+4", created_at: "2026-06-07T00:00:02Z" },
    { id: "r46-r24-a2", seq: 4, role: "assistant", body: "8", created_at: "2026-06-07T00:00:03Z" },
    { id: "r46-r24-u3", seq: 5, role: "user", body: "what is 8+8", created_at: "2026-06-07T00:00:04Z" },
    { id: "r46-r24-a3", seq: 6, role: "assistant", body: "16", created_at: "2026-06-07T00:00:05Z" },
    { id: "r46-r24-u4", seq: 7, role: "user", body: "what is 16+16", created_at: "2026-06-07T00:00:06Z" },
    // turn-4 assistant NOT yet arrived
  ],
  has_more: false,
};

// SSE: no user_input events; CLI replays turns 1+2 (startSeq=2 and startSeq=4,
// matching DB seq values so they are dropped by histAssistantSeqs), then emits
// the fresh turn-3 completion (startSeq=6, not in DB yet → kept).
export const THREE_TURN_COMPLETED_NO_INPROGRESS_SSE = [
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "text", sequence: 2, payload: { type: "text", text: "4" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 3, payload: { type: "turn_complete", stop_reason: "end_turn" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "text", sequence: 4, payload: { type: "text", text: "8" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 5, payload: { type: "turn_complete", stop_reason: "end_turn" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "text", sequence: 6, payload: { type: "text", text: "16" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 7, payload: { type: "turn_complete", stop_reason: "end_turn" } })}`,
  "",
  "",
].join("\n");

// ─── 2-turn R27 scenario (RUSAA-1949) ──────────────────────────────────────
// Session: turn-1 complete, turn-2 just completed.
// CLI restart with NEW sequence numbers (does NOT reuse DB seqs).
// The SSE stream emits the fresh turn-2 answer FIRST (lower seqs), then
// the CLI replays turn-1's response SECOND (higher seqs).
// The existing seq-based filter cannot drop the replay (new seq not in histAssistantSeqs);
// the content-based filter catches it because the replay text matches historical ass-1.
//
// Bug (R26 under-correction): position-based slice kept the SECOND item = replay of
// turn-1's "Did you mean 2+2?" instead of fresh turn-2's "Then 2+2=4".
// Fix: content-match filter drops the replay before dedupeAssistantsPerSegment runs.

// DB state: turn-1 fully persisted; turn-2 user stored, assistant not yet.
// histAssistantSeqs = {2} (only ass-1 in DB).
export const LIST_MESSAGES_R27_TURN2_IN_PROGRESS = {
  messages: [
    { id: "r49-u1", seq: 1, role: "user", body: "what is 2+@", created_at: "2026-06-07T00:00:00Z" },
    { id: "r49-a1", seq: 2, role: "assistant", body: "Did you mean 2+2? That equals 4.", created_at: "2026-06-07T00:00:01Z" },
    { id: "r49-u2", seq: 3, role: "user", body: "@ is 2", created_at: "2026-06-07T00:00:02Z" },
    // turn-2 assistant NOT yet persisted
  ],
  has_more: false,
};

// SSE: no user_input events; CLI uses NEW seqs (not matching DB).
// Fresh turn-2 answer arrives FIRST (seq=100), then turn-1 replay arrives SECOND (seq=102).
// This is the R27 bug ordering: fresh first, replay second.
export const TWO_TURN_R27_FRESH_FIRST_REPLAY_SECOND_SSE = [
  // Fresh answer to "@ is 2": "Then 2+2=4." arrives first (seq=100).
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "text", sequence: 100, payload: { type: "text", text: "Then 2+2=4." } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 101, payload: { type: "turn_complete", stop_reason: "end_turn" } })}`,
  "",
  // CLI replay of turn-1 ("Did you mean 2+2? That equals 4.") arrives second (seq=102).
  // Same text as DB ass-1 body → content filter must drop it.
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "text", sequence: 102, payload: { type: "text", text: "Did you mean 2+2? That equals 4." } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 103, payload: { type: "turn_complete", stop_reason: "end_turn" } })}`,
  "",
  "",
].join("\n");

// Same as above but turn-3's stream is still in-progress (no turn_complete after "16").
// The CLI replays turns 1+2 as completed, then turn-3 is still streaming.
export const THREE_TURN_MIDSTREAM_NO_INPROGRESS_SSE = [
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "text", sequence: 2, payload: { type: "text", text: "4" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 3, payload: { type: "turn_complete", stop_reason: "end_turn" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "text", sequence: 4, payload: { type: "text", text: "8" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 5, payload: { type: "turn_complete", stop_reason: "end_turn" } })}`,
  "",
  // Turn-3 still streaming — no turn_complete.
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "text", sequence: 6, payload: { type: "text", text: "16" } })}`,
  "",
  "",
].join("\n");
