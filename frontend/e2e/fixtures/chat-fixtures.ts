export const CHAT_SESSION_ID = "chat-session-001";

export const CHAT_SESSION_FIXTURE = {
  id: CHAT_SESSION_ID,
  tenant_id: "tenant-1",
  user_id: "user-1",
  runtime: "claude_code" as const,
  status: "active" as const,
  trace_id: "trace-abc123",
  created_at: "2026-06-03T00:00:00Z",
  last_activity_at: "2026-06-03T00:00:01Z",
  ended_at: null,
};

export const LIST_SESSIONS_EMPTY = { sessions: [] };

export const LIST_SESSIONS_ONE = { sessions: [CHAT_SESSION_FIXTURE] };

export const CREATE_SESSION_RESPONSE = { session_id: CHAT_SESSION_ID };

export const SEND_MESSAGE_RESPONSE = { message_id: "msg-001" };

// Full exchange: user_input → tool_use → tool_result → text
export const FULL_EXCHANGE_SSE = [
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "user_input",
    sequence: 1,
    payload: { type: "user_input", text: "List files in the current directory" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "tool_use",
    sequence: 2,
    payload: {
      type: "tool_use",
      id: "tool-001",
      name: "list_directory",
      input: { path: "." },
    },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "tool_result",
    sequence: 3,
    payload: {
      type: "tool_result",
      tool_use_id: "tool-001",
      content: ["file1.txt", "file2.rs"],
      is_error: false,
    },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 4,
    payload: { type: "text", text: "Here are the files in the current directory." },
  })}`,
  "",
  "",
].join("\n");

export const SESSION_ERROR_SSE = [
  "event: session.error",
  `data: ${JSON.stringify({
    error: "timeout",
    status: "504",
    message: "Session timed out",
  })}`,
  "",
  "",
].join("\n");

export const LIST_MESSAGES_TWO_TURNS = {
  messages: [
    {
      id: "msg-001",
      seq: 1,
      role: "user",
      body: "Hello from reload test",
      created_at: "2026-06-03T00:00:00Z",
    },
    {
      id: "msg-002",
      seq: 2,
      role: "assistant",
      body: "Hello back! I remember your message.",
      created_at: "2026-06-03T00:00:01Z",
    },
    {
      id: "msg-003",
      seq: 3,
      role: "user",
      body: "Second message",
      created_at: "2026-06-03T00:00:02Z",
    },
    {
      id: "msg-004",
      seq: 4,
      role: "assistant",
      body: "Got your second message.",
      created_at: "2026-06-03T00:00:03Z",
    },
  ],
  has_more: false,
};

export const LIST_MESSAGES_EMPTY = {
  messages: [] as never[],
  has_more: false,
};

// Two-turn history using "What MCP tools…" prompt (matches AC3 reload assertion).
export const LIST_MESSAGES_MCP_EXCHANGE = {
  messages: [
    {
      id: "msg-mcp-001",
      seq: 1,
      role: "user",
      body: "What MCP tools are available?",
      created_at: "2026-06-03T00:00:00Z",
    },
    {
      id: "msg-mcp-002",
      seq: 2,
      role: "assistant",
      body: "The following MCP tools are registered: bash, read_file, write_file.",
      created_at: "2026-06-03T00:00:01Z",
    },
  ],
  has_more: false,
};

// SSE fixture for a new exchange sent after prior history is already loaded.
// Represents: user sends "How do I use the bash tool?", assistant replies.
export const MID_SEND_SSE = [
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "user_input",
    sequence: 1,
    payload: { type: "user_input", text: "How do I use the bash tool?" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 2,
    payload: { type: "text", text: "You can use the bash tool to run shell commands in the workspace." },
  })}`,
  "",
  "",
].join("\n");

// Tool-use exchange stored in the new JSON content-block body format (post-1896).
// body is a JSON array: [tool_use block, tool_result block, text block].
export const LIST_MESSAGES_WITH_TOOL_USE = {
  messages: [
    {
      id: "msg-tool-u1",
      seq: 1,
      role: "user",
      body: "Search for recent Rust news",
      created_at: "2026-06-04T00:00:00Z",
    },
    {
      id: "msg-tool-a1",
      seq: 2,
      role: "assistant",
      body: JSON.stringify([
        {
          type: "tool_use",
          id: "tu-001",
          name: "mcp__rust_brain__search_demo",
          input: { q: "recent Rust news" },
        },
        {
          type: "tool_result",
          tool_use_id: "tu-001",
          content: "Found 5 results for recent Rust news",
          is_error: false,
        },
        {
          type: "text",
          text: "Here are the recent Rust news results.",
        },
      ]),
      created_at: "2026-06-04T00:00:01Z",
    },
  ],
  has_more: false,
};

// Simulates the SSE echo race: assistant tokens stream for a SECOND turn WITHOUT
// a preceding user_input echo.  buildTranscript accumulates these as an
// in-progress (inProgress: true) assistant item.  Used to test that the
// optimistic pending bubble is slotted BEFORE this in-progress item.
export const IN_PROGRESS_NO_ECHO_SSE = [
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 3,
    payload: { type: "text", text: "I'm analyzing your request now..." },
  })}`,
  "",
  "",
].join("\n");

// Simulates the turn-2 stale-inProgress race: a completed turn-1 exchange
// (user_input + text) where inProgress is never cleared because no subsequent
// user_input arrived.  buildTranscript marks the trailing assistant as
// inProgress: true even though turn-1 is fully done.
// Used to assert that the turn-2 pending bubble is placed AFTER assistant-1,
// NOT slotted before it.
export const TURN1_COMPLETE_STALE_INPROGRESS_SSE = [
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "user_input",
    sequence: 1,
    payload: { type: "user_input", text: "what are the tools available" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 2,
    payload: { type: "text", text: "Here are the available tools." },
  })}`,
  "",
  "",
].join("\n");

// Simulates two completed turns including the turn-2 user_input echo.
// buildTranscript produces: [user-1, assistant-1, user-2, assistant-2(inProgress)]
// Used to verify AC3: after turn-2 echo arrives, ordering is stable and
// no visual reshuffle of items 1 (user-1) and 2 (assistant-1) occurs.
export const TURN2_WITH_ECHO_SSE = [
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "user_input",
    sequence: 1,
    payload: { type: "user_input", text: "what are the tools available" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 2,
    payload: { type: "text", text: "Here are the available tools." },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "user_input",
    sequence: 3,
    payload: { type: "user_input", text: "Tell me about ownership" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 4,
    payload: { type: "text", text: "Ownership is Rust's key memory feature." },
  })}`,
  "",
  "",
].join("\n");

// Edge case (RUSAA-1907): SSE joined after user_input was emitted — only text
// content arrives.  buildTranscript produces [assistant-1(inProgress)] with
// no user_input item.  Historical DB supplies user-1.  Together base becomes
// [user-1-hist, assistant-1(inProgress)].  The slot predicate must NOT insert
// the turn-2 pending bubble at position 1 (between user-1 and assistant-1).
export const TURN1_ASSISTANT_ONLY_SSE = [
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 2,
    payload: { type: "text", text: "Here are the available tools." },
  })}`,
  "",
  "",
].join("\n");

// Historical with only user-1 — assistant row not yet flushed to DB.
// Pairs with TURN1_ASSISTANT_ONLY_SSE to reproduce the edge case.
export const LIST_MESSAGES_TURN1_USER_ONLY = {
  messages: [
    {
      id: "msg-001",
      seq: 1,
      role: "user",
      body: "what are the tools available",
      created_at: "2026-06-03T00:00:00Z",
    },
  ],
  has_more: false,
};

// Sequential turn-2 reconnect: SSE joined after turn-1 completed and the
// connection dropped. Only assistant-2 streaming tokens arrive — no user_input
// echoes.  Used to test RUSAA-1915: pending user-2 must slot BEFORE the
// in-progress assistant-2, not after it.
export const TURN2_ASSISTANT_ONLY_SSE = [
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 4,
    payload: { type: "text", text: "Lift is a higher-order function that maps a regular function into a functor." },
  })}`,
  "",
  "",
].join("\n");

// Historical with only user-1 "what is monad" — assistant-1 not yet flushed to DB.
// Pairs with TURN2_ASSISTANT_ONLY_SSE to reproduce the RUSAA-1915 edge case where
// the secondary guard mis-fires: candidateSlot-1 is user-1-hist (kind "user"),
// but user-1 was already answered; the in-progress is for user-2 (pending).
export const LIST_MESSAGES_MONAD_USER_ONLY = {
  messages: [
    {
      id: "msg-seq1",
      seq: 1,
      role: "user",
      body: "what is monad",
      created_at: "2026-06-03T00:00:00Z",
    },
  ],
  has_more: false,
};

// SSE fixture that starts streaming assistant tokens immediately (simulates assistant-1
// in-progress with no user_input echo). Used to test composer queue behaviour.
export const STREAMING_ASSISTANT_SSE = [
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 2,
    payload: { type: "text", text: "I am currently processing your request…" },
  })}`,
  "",
  "",
].join("\n");

// SSE fixture with a completed full exchange — used to test that the queue drains
// after the assistant finishes streaming (inProgress transitions to false).
export const COMPLETED_EXCHANGE_SSE = [
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "user_input",
    sequence: 1,
    payload: { type: "user_input", text: "hello" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 2,
    payload: { type: "text", text: "Hello! How can I help you today?" },
  })}`,
  "",
  "",
].join("\n");

// A single completed turn that includes the turn_complete event.
// buildTranscript must flush the pending assistant so inProgress is never set,
// which means assistantStreaming = false and the composer button reads "Send".
export const SINGLE_TURN_COMPLETE_SSE = [
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "user_input",
    sequence: 1,
    payload: { type: "user_input", text: "what is ownership?" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 2,
    payload: { type: "text", text: "Ownership is Rust's core memory safety mechanism." },
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
  "",
].join("\n");

// Two complete turns, each terminated by turn_complete.
// buildTranscript must produce: [user-1, assistant-1, user-2, assistant-2]
// Used to verify multi-turn queue-drain ordering when turn_complete is present.
export const TURN2_WITH_TURN_COMPLETE_SSE = [
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "user_input",
    sequence: 1,
    payload: { type: "user_input", text: "what are the tools available" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 2,
    payload: { type: "text", text: "Here are the available tools." },
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
    payload: { type: "user_input", text: "Tell me about ownership" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 5,
    payload: { type: "text", text: "Ownership is Rust's key memory feature." },
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
  "",
].join("\n");


// SSE fixture for a tool-using turn with the intermediate turn_complete(tool_use)
// that claude-code emits between tool_use and tool_result events.
// Regression fixture for RUSAA-1951: the intermediate turn_complete must NOT
// split tool_use and tool_result into separate assistant items.
export const TOOL_USE_WITH_INTERMEDIATE_TURN_COMPLETE_SSE = [
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "user_input",
    sequence: 1,
    payload: { type: "user_input", text: "Search for rust async patterns" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "tool_use",
    sequence: 2,
    payload: {
      type: "tool_use",
      id: "tu-live-001",
      name: "mcp__rust_brain__recall",
      input: { tags: ["rust", "async"] },
    },
  })}`,
  "",
  // Intermediate turn_complete emitted by claude-code when model pauses for tool execution.
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
      tool_use_id: "tu-live-001",
      content: "Found 3 async patterns: tokio, async-std, smol",
      is_error: false,
    },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 5,
    payload: { type: "text", text: "Here are the async patterns I found." },
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
  "",
].join("\n");

// Split-batch history: tool_use stored in one DB row, tool_result+text in next.
// Simulates what happens when the agent-runner flushes mid-turn because the tool
// takes time to execute (events arrive in separate HTTP batches).
export const LIST_MESSAGES_SPLIT_TOOL_TURN = {
  messages: [
    {
      id: "msg-split-u1",
      seq: 1,
      role: "user",
      body: "Search for rust async patterns",
      created_at: "2026-06-04T00:00:00Z",
    },
    {
      id: "msg-split-a1",
      seq: 2,
      role: "assistant",
      // First batch: only the tool_use (tool execution hadn't completed yet)
      body: JSON.stringify([
        {
          type: "tool_use",
          id: "tu-reload-001",
          name: "mcp__rust_brain__recall",
          input: { tags: ["rust", "async"] },
        },
      ]),
      created_at: "2026-06-04T00:00:01Z",
    },
    {
      id: "msg-split-a2",
      seq: 3,
      role: "assistant",
      // Second batch: tool_result + final text
      body: JSON.stringify([
        {
          type: "tool_result",
          tool_use_id: "tu-reload-001",
          content: "Found 3 async patterns: tokio, async-std, smol",
          is_error: false,
        },
        {
          type: "text",
          text: "Here are the async patterns I found.",
        },
      ]),
      created_at: "2026-06-04T00:00:02Z",
    },
  ],
  has_more: false,
};
