// R29 regression fixtures (RUSAA-1962): turn-2 send wipes prior-turn transcript.
// Extracted from chat-mock-api-cli-restart.ts to keep that file under the 600-line cap.

import { CHAT_SESSION_ID } from "./chat-mock-api";

// ─── R29 regression scenario (RUSAA-1962) ──────────────────────────────────
// Session: turn-1 WITH tool calls (tool_use + intermediate turn_complete(tool_use)
// + tool_result + text + turn_complete(end_turn)), then turn-2 starts streaming.
//
// CLI restart pattern (no user_input events in SSE): the CLI replays turn-1's
// tool-use sequence using the ORIGINAL seq numbers (2–6), then starts streaming
// turn-2's new tool call (seq=7+).
//
// PR #734 regression: turn_complete(stop_reason="tool_use") in the replay is NOT
// flushed (correct fix for live rendering), so the replay produces ONE flushed
// assistant item (startSeq=2). histAssistantSeqs={2} correctly drops it. BUT in
// the !firstLiveUser path, dedupeAssistantsPerSegment was called with
// [...histItems, extraLive(inProgress)], placing assistant-2(inProgress) in
// user-1's segment alongside assistant-1(hist). The "has inProgress → keep only
// last inProgress" rule then drops the completed assistant-1, wiping turn-1
// content from the transcript.

// DB state: turn-1 complete (assistant-1 seq=2 matches replay startSeq → dropped
// by histAssistantSeqs filter). Turn-2 user stored but assistant not yet persisted.
export const LIST_MESSAGES_R29_TOOL_USE_TURN1_TURN2_USER = {
  messages: [
    {
      id: "r62-u1",
      seq: 1,
      role: "user",
      body: "in rust brain",
      created_at: "2026-06-09T00:00:00Z",
    },
    {
      id: "r62-a1",
      seq: 2,
      role: "assistant",
      body: JSON.stringify([
        {
          type: "tool_use",
          id: "tu-r62-001",
          name: "mcp_rust_brain_search_items",
          input: { q: "rust brain" },
        },
        {
          type: "tool_result",
          tool_use_id: "tu-r62-001",
          content: "Found 3 rust brain items",
          is_error: false,
        },
        { type: "text", text: "Here is what I found in rust brain." },
      ]),
      created_at: "2026-06-09T00:00:01Z",
    },
    {
      id: "r62-u2",
      seq: 7,
      role: "user",
      body: "show me more",
      created_at: "2026-06-09T00:00:02Z",
    },
    // turn-2 assistant NOT yet persisted
  ],
  has_more: false,
};

// SSE: no user_input events (CLI restarted after turn-2 user message was processed).
// CLI replays turn-1 tool-use (seq 2–6), then starts turn-2's fresh tool call (seq 7+).
// Turn-2 stays in-progress (no final turn_complete — tool still executing).
export const TOOL_USE_REPLAY_THEN_TURN2_STREAMING_SSE = [
  // Replay of turn-1: tool_use + intermediate turn_complete(tool_use) + tool_result + text + end_turn
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "tool_use",
    sequence: 2,
    payload: {
      type: "tool_use",
      id: "tu-r62-001",
      name: "mcp_rust_brain_search_items",
      input: { q: "rust brain" },
    },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "turn_complete",
    sequence: 3,
    payload: { type: "turn_complete", stop_reason: "tool_use" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "tool_result",
    sequence: 4,
    payload: {
      type: "tool_result",
      tool_use_id: "tu-r62-001",
      content: "Found 3 rust brain items",
      is_error: false,
    },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 5,
    payload: { type: "text", text: "Here is what I found in rust brain." },
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
  // Fresh turn-2: new tool call streaming (still in-progress — no final turn_complete)
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "tool_use",
    sequence: 7,
    payload: {
      type: "tool_use",
      id: "tu-r62-002",
      name: "mcp_rust_brain_get_item",
      input: { id: "item-1" },
    },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "turn_complete",
    sequence: 8,
    payload: { type: "turn_complete", stop_reason: "tool_use" },
  })}`,
  "",
  "",
].join("\n");

// AC3: three-turn history — turns 1+2 complete, turn-3 user in DB, turn-3 streaming.
export const LIST_MESSAGES_R29_THREE_TURNS_TURN3_USER = {
  messages: [
    {
      id: "r62-t3-u1",
      seq: 1,
      role: "user",
      body: "in rust brain",
      created_at: "2026-06-09T00:00:00Z",
    },
    {
      id: "r62-t3-a1",
      seq: 2,
      role: "assistant",
      body: JSON.stringify([
        {
          type: "tool_use",
          id: "tu-r62-001",
          name: "mcp_rust_brain_search_items",
          input: { q: "rust brain" },
        },
        {
          type: "tool_result",
          tool_use_id: "tu-r62-001",
          content: "Found 3 rust brain items",
          is_error: false,
        },
        { type: "text", text: "Here is what I found in rust brain." },
      ]),
      created_at: "2026-06-09T00:00:01Z",
    },
    {
      id: "r62-t3-u2",
      seq: 7,
      role: "user",
      body: "show me more",
      created_at: "2026-06-09T00:00:02Z",
    },
    {
      id: "r62-t3-a2",
      seq: 9,
      role: "assistant",
      body: JSON.stringify([
        {
          type: "tool_use",
          id: "tu-r62-002",
          name: "mcp_rust_brain_get_item",
          input: { id: "item-1" },
        },
        {
          type: "tool_result",
          tool_use_id: "tu-r62-002",
          content: "Item details: ...",
          is_error: false,
        },
        { type: "text", text: "Here are the details for item-1." },
      ]),
      created_at: "2026-06-09T00:00:03Z",
    },
    {
      id: "r62-t3-u3",
      seq: 14,
      role: "user",
      body: "what else?",
      created_at: "2026-06-09T00:00:04Z",
    },
    // turn-3 assistant NOT yet persisted
  ],
  has_more: false,
};

// SSE for AC3: CLI replays turns 1+2 (no user_input), then turn-3 streams.
export const TOOL_USE_TWO_REPLAYS_THEN_TURN3_STREAMING_SSE = [
  // Replay of turn-1 (seq 2–6)
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "tool_use", sequence: 2, payload: { type: "tool_use", id: "tu-r62-001", name: "mcp_rust_brain_search_items", input: { q: "rust brain" } } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 3, payload: { type: "turn_complete", stop_reason: "tool_use" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "tool_result", sequence: 4, payload: { type: "tool_result", tool_use_id: "tu-r62-001", content: "Found 3 rust brain items", is_error: false } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "text", sequence: 5, payload: { type: "text", text: "Here is what I found in rust brain." } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 6, payload: { type: "turn_complete", stop_reason: "end_turn" } })}`,
  "",
  // Replay of turn-2 (seq 9–13)
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "tool_use", sequence: 9, payload: { type: "tool_use", id: "tu-r62-002", name: "mcp_rust_brain_get_item", input: { id: "item-1" } } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 10, payload: { type: "turn_complete", stop_reason: "tool_use" } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "tool_result", sequence: 11, payload: { type: "tool_result", tool_use_id: "tu-r62-002", content: "Item details: ...", is_error: false } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "text", sequence: 12, payload: { type: "text", text: "Here are the details for item-1." } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 13, payload: { type: "turn_complete", stop_reason: "end_turn" } })}`,
  "",
  // Fresh turn-3: new tool call streaming
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "tool_use", sequence: 14, payload: { type: "tool_use", id: "tu-r62-003", name: "mcp_rust_brain_list", input: {} } })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({ session_id: CHAT_SESSION_ID, event_type: "turn_complete", sequence: 15, payload: { type: "turn_complete", stop_reason: "tool_use" } })}`,
  "",
  "",
].join("\n");
