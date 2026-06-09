// R31 regression fixtures (RUSAA-1966): prior turn text output disappears when new
// turn completes, in sessions where turn N has text output before tool calls.
//
// Root cause: buildTranscriptFromHistory creates TWO AssistantTranscriptItems for turn N
// when the DB has a text-only row followed by a tool_use row (the agent-runner flushed
// the text response before the tool call started). When dedupeAssistantsPerSegment runs
// after hasLiveInProgress transitions to false, it sees 2 completed items in turn N's
// segment and drops the text item (replayCount ≥ 1).
//
// buildTranscript has the same issue when the SSE stream has turn_complete(end_turn)
// between text and tool_use events: the text is flushed as a separate AssistantItem.
//
// Fix (transcript.ts):
//   1. buildTranscriptFromHistory: extend split-batch merge to also cover the case
//      where the NEXT row starts with tool_use (not only when prev ends with tool_use).
//   2. buildTranscript: post-process to merge consecutive assistant items where the
//      second starts with tool_use, same as the history fix.

import { CHAT_SESSION_ID } from "./chat-mock-api";

// ─── Shared DB rows ──────────────────────────────────────────────────────────
//
// Turn 1: simple text response (seq=2).
// Turn 2: text THEN tool_use in SEPARATE DB rows — text-only row (seq=5), tools row (seq=7).
// Turn 3: simple text response (seq=12).
// histAssistantSeqs will be {2, 5, 7, 12} → size=4, triggering replayCount=2 → both dropped
// (without fix), or size=1 → replayCount=1 → text dropped but tools kept (race-window variant).

export const LIST_MESSAGES_R31_THREE_TURNS_SPLIT_TURN2 = {
  messages: [
    {
      id: "r31-u1",
      seq: 1,
      role: "user",
      body: "what is rust brain?",
      created_at: "2026-06-09T00:00:00Z",
    },
    {
      id: "r31-a1",
      seq: 2,
      role: "assistant",
      body: JSON.stringify([
        { type: "text", text: "RustBrain is a code intelligence platform." },
      ]),
      created_at: "2026-06-09T00:00:01Z",
    },
    {
      id: "r31-u2",
      seq: 4,
      role: "user",
      body: "search for rust examples",
      created_at: "2026-06-09T00:00:02Z",
    },
    // Split DB rows for turn 2: text row SEPARATE from tool_use row.
    // This is the RUSAA-1966 DB structure — the agent-runner flushed the initial text
    // response before the tool call started, creating two rows for the same turn.
    {
      id: "r31-a2-text",
      seq: 5,
      role: "assistant",
      body: JSON.stringify([
        { type: "text", text: "Searching for rust examples..." },
      ]),
      created_at: "2026-06-09T00:00:03Z",
    },
    {
      id: "r31-a2-tools",
      seq: 7,
      role: "assistant",
      body: JSON.stringify([
        {
          type: "tool_use",
          id: "tu-r31-001",
          name: "mcp__rust_brain__search_items",
          input: { q: "rust" },
        },
        {
          type: "tool_result",
          tool_use_id: "tu-r31-001",
          content: "Found 5 rust examples",
          is_error: false,
        },
      ]),
      created_at: "2026-06-09T00:00:04Z",
    },
    {
      id: "r31-u3",
      seq: 11,
      role: "user",
      body: "show details",
      created_at: "2026-06-09T00:00:05Z",
    },
    {
      id: "r31-a3",
      seq: 12,
      role: "assistant",
      body: JSON.stringify([
        { type: "text", text: "Here are the details for the first result." },
      ]),
      created_at: "2026-06-09T00:00:06Z",
    },
  ],
  has_more: false,
};

// ─── !firstLiveUser path (CLI restart) ───────────────────────────────────────
//
// SSE: no user_input events. CLI replays all turns. Turn 2 replay has
// turn_complete(end_turn) between the text event and the tool_use event — matching the
// original DB split. All turns complete (turn 3 included) → hasLiveInProgress=false.
//
// Without fix: dedup sees user2 segment = [asst2_text_hist, asst2_tools_hist]
//              → completedCount=2, replayCount=min(4,2)=2 → BOTH dropped.
// With fix:    buildTranscriptFromHistory merges asst2_text+asst2_tools → 1 item
//              → completedCount=1 → keep.
export const R31_CLI_RESTART_SPLIT_TURN2_ALL_COMPLETE_SSE = [
  // Replay turn 1: text only
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 2,
    payload: { type: "text", text: "RustBrain is a code intelligence platform." },
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
  // Replay turn 2: text, then end_turn, then tool_use — the RUSAA-1966 split pattern
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 5,
    payload: { type: "text", text: "Searching for rust examples..." },
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
    event_type: "tool_use",
    sequence: 7,
    payload: {
      type: "tool_use",
      id: "tu-r31-001",
      name: "mcp__rust_brain__search_items",
      input: { q: "rust" },
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
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "tool_result",
    sequence: 9,
    payload: {
      type: "tool_result",
      tool_use_id: "tu-r31-001",
      content: "Found 5 rust examples",
      is_error: false,
    },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "turn_complete",
    sequence: 10,
    payload: { type: "turn_complete", stop_reason: "end_turn" },
  })}`,
  "",
  // Replay turn 3: text only (full completion)
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 12,
    payload: { type: "text", text: "Here are the details for the first result." },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "turn_complete",
    sequence: 13,
    payload: { type: "turn_complete", stop_reason: "end_turn" },
  })}`,
  "",
  "",
].join("\n");

// ─── !firstLiveUser path — turn 3 in-progress → then completes ───────────────
//
// Same as above but turn 3 is still in-progress when first seen (hasLiveInProgress=true,
// dedup skipped → text visible), then completes (hasLiveInProgress=false → dedup runs).
// The test SSE delivers the complete stream so we observe the final "completed" state.
// This is the exact sequence that triggers the regression.
export const R31_CLI_RESTART_SPLIT_TURN2_TURN3_STREAMING_SSE = [
  // Replay turn 1
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 2,
    payload: { type: "text", text: "RustBrain is a code intelligence platform." },
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
  // Replay turn 2: text + end_turn + tool_use (RUSAA-1966 pattern)
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 5,
    payload: { type: "text", text: "Searching for rust examples..." },
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
    event_type: "tool_use",
    sequence: 7,
    payload: {
      type: "tool_use",
      id: "tu-r31-001",
      name: "mcp__rust_brain__search_items",
      input: { q: "rust" },
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
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "tool_result",
    sequence: 9,
    payload: {
      type: "tool_result",
      tool_use_id: "tu-r31-001",
      content: "Found 5 rust examples",
      is_error: false,
    },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "turn_complete",
    sequence: 10,
    payload: { type: "turn_complete", stop_reason: "end_turn" },
  })}`,
  "",
  // Fresh turn 3: streams then COMPLETES — this is the trigger for the regression
  // (hasLiveInProgress flips from true to false, dedup runs for the first time)
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 12,
    payload: { type: "text", text: "Here are the details for the first result." },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "turn_complete",
    sequence: 13,
    payload: { type: "turn_complete", stop_reason: "end_turn" },
  })}`,
  "",
  "",
].join("\n");

// ─── firstLiveUser path (live session) ───────────────────────────────────────
//
// SSE with user_input events: full live session where turn 2 emits text(end_turn)
// then tool_use in separate SSE events. After turn 3 completes, buildTranscript
// would produce 2 items for turn 2; dedupeAssistantsPerSegment would drop the text.
//
// Without fix: liveDeduped user2 segment = [asst2_text(startSeq=5), asst2_tools(startSeq=7)]
//              → completedCount=2, replayCount=min(4,2)=2 → BOTH dropped.
// With fix:    buildTranscript post-process merges → 1 item → completedCount=1 → keep.
export const LIST_MESSAGES_R31_LIVE_SPLIT_TURN2 = {
  messages: [
    {
      id: "r31-lv-u1",
      seq: 1,
      role: "user",
      body: "what is rust brain?",
      created_at: "2026-06-09T00:00:00Z",
    },
    {
      id: "r31-lv-a1",
      seq: 2,
      role: "assistant",
      body: JSON.stringify([
        { type: "text", text: "RustBrain is a code intelligence platform." },
      ]),
      created_at: "2026-06-09T00:00:01Z",
    },
    {
      id: "r31-lv-u2",
      seq: 4,
      role: "user",
      body: "search for rust examples",
      created_at: "2026-06-09T00:00:02Z",
    },
    {
      id: "r31-lv-a2-text",
      seq: 5,
      role: "assistant",
      body: JSON.stringify([{ type: "text", text: "Searching for rust examples..." }]),
      created_at: "2026-06-09T00:00:03Z",
    },
    {
      id: "r31-lv-a2-tools",
      seq: 7,
      role: "assistant",
      body: JSON.stringify([
        {
          type: "tool_use",
          id: "tu-r31-lv-001",
          name: "mcp__rust_brain__search_items",
          input: { q: "rust" },
        },
        {
          type: "tool_result",
          tool_use_id: "tu-r31-lv-001",
          content: "Found 5 rust examples",
          is_error: false,
        },
      ]),
      created_at: "2026-06-09T00:00:04Z",
    },
    {
      id: "r31-lv-u3",
      seq: 11,
      role: "user",
      body: "show details",
      created_at: "2026-06-09T00:00:05Z",
    },
    {
      id: "r31-lv-a3",
      seq: 12,
      role: "assistant",
      body: JSON.stringify([
        { type: "text", text: "Here are the details for the first result." },
      ]),
      created_at: "2026-06-09T00:00:06Z",
    },
  ],
  has_more: false,
};

export const R31_LIVE_SESSION_SPLIT_TURN2_ALL_COMPLETE_SSE = [
  // Turn 1
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "user_input",
    sequence: 1,
    payload: { type: "user_input", text: "what is rust brain?" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 2,
    payload: { type: "text", text: "RustBrain is a code intelligence platform." },
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
  // Turn 2: user_input, text response (end_turn), then tool_use in next step
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "user_input",
    sequence: 4,
    payload: { type: "user_input", text: "search for rust examples" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 5,
    payload: { type: "text", text: "Searching for rust examples..." },
  })}`,
  "",
  // end_turn fires after the text — tool_use follows as the next agentic step
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
    event_type: "tool_use",
    sequence: 7,
    payload: {
      type: "tool_use",
      id: "tu-r31-lv-001",
      name: "mcp__rust_brain__search_items",
      input: { q: "rust" },
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
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "tool_result",
    sequence: 9,
    payload: {
      type: "tool_result",
      tool_use_id: "tu-r31-lv-001",
      content: "Found 5 rust examples",
      is_error: false,
    },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "turn_complete",
    sequence: 10,
    payload: { type: "turn_complete", stop_reason: "end_turn" },
  })}`,
  "",
  // Turn 3: completes — this triggers dedupeAssistantsPerSegment to run and
  // (without fix) drops turn 2's text from user2's segment
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "user_input",
    sequence: 11,
    payload: { type: "user_input", text: "show details" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 12,
    payload: { type: "text", text: "Here are the details for the first result." },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "turn_complete",
    sequence: 13,
    payload: { type: "turn_complete", stop_reason: "end_turn" },
  })}`,
  "",
  "",
].join("\n");
