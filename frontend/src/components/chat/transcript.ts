// Transcript types and reducer for building a conversational view from SSE events.

import type { StreamedEvent } from "@/hooks/useEventStream";
import type { ChatMessage, ChatSessionEventEnvelope, ChatRuntimePayload } from "@/lib/chat-api";

export interface UserTranscriptItem {
  kind: "user";
  id: string;
  text: string;
  seq: number;
}

export interface AssistantTranscriptItem {
  kind: "assistant";
  id: string;
  items: ReadonlyArray<AssistantItem>;
  // True only on the trailing pending turn that has not yet been flushed by a
  // user_input event.  Used by ChatPage to slot optimistic pending bubbles in
  // chronological order during the SSE echo race window.
  inProgress?: boolean;
  // Sequence number of the first SSE event for this assistant turn. Used by
  // ChatPage to deduplicate live items against history by matching DB seq values.
  startSeq?: number;
}

export interface ErrorTranscriptItem {
  kind: "error";
  id: string;
  message: string;
  code?: string;
}

export type TranscriptItem =
  | UserTranscriptItem
  | AssistantTranscriptItem
  | ErrorTranscriptItem;

export type AssistantItem =
  | { type: "text"; text: string; seq: number }
  | { type: "thinking"; thinking: string; seq: number }
  | { type: "tool_use"; id: string; name: string; input: unknown; seq: number }
  | { type: "tool_result"; toolUseId: string; content: unknown; isError: boolean; seq: number }
  | { type: "error"; message: string; code?: string; seq: number };

interface ReducerState {
  readonly items: ReadonlyArray<TranscriptItem>;
  readonly pendingAssistant: ReadonlyArray<AssistantItem> | null;
  readonly pendingStartSeq: number | null;
  readonly counter: number;
}

function parseJson<T>(s: string): T | null {
  try {
    return JSON.parse(s) as T;
  } catch {
    return null;
  }
}

function isChatSessionEventEnvelope(v: unknown): v is ChatSessionEventEnvelope {
  if (typeof v !== "object" || v === null) return false;
  const o = v as Record<string, unknown>;
  return (
    typeof o.sequence === "number" &&
    typeof o.payload === "object" &&
    o.payload !== null &&
    typeof (o.payload as Record<string, unknown>).type === "string"
  );
}

function appendAssistantItem(
  pending: ReadonlyArray<AssistantItem>,
  payload: ChatRuntimePayload,
  sequence: number,
): ReadonlyArray<AssistantItem> {
  if (payload.type === "text") {
    const last = pending[pending.length - 1];
    if (last?.type === "text") {
      // Merge consecutive text tokens into one entry (immutable update).
      return [
        ...pending.slice(0, -1),
        { type: "text", text: last.text + payload.text, seq: last.seq },
      ];
    }
    return [...pending, { type: "text", text: payload.text, seq: sequence }];
  }
  if (payload.type === "thinking") {
    return [...pending, { type: "thinking", thinking: payload.thinking, seq: sequence }];
  }
  if (payload.type === "tool_use") {
    return [
      ...pending,
      { type: "tool_use", id: payload.id, name: payload.name, input: payload.input, seq: sequence },
    ];
  }
  if (payload.type === "tool_result") {
    return [
      ...pending,
      {
        type: "tool_result",
        toolUseId: payload.tool_use_id,
        content: payload.content,
        isError: payload.is_error,
        seq: sequence,
      },
    ];
  }
  if (payload.type === "error") {
    const errorItem: AssistantItem = payload.code !== undefined
      ? { type: "error", message: payload.message, code: payload.code, seq: sequence }
      : { type: "error", message: payload.message, seq: sequence };
    return [...pending, errorItem];
  }
  return pending;
}

function flushPendingAssistant(state: ReducerState): ReducerState {
  if (state.pendingAssistant === null || state.pendingAssistant.length === 0) {
    return { ...state, pendingAssistant: null, pendingStartSeq: null };
  }
  const assistantItem: AssistantTranscriptItem = {
    kind: "assistant",
    id: `a-${state.counter}`,
    items: state.pendingAssistant,
    ...(state.pendingStartSeq !== null ? { startSeq: state.pendingStartSeq } : {}),
  };
  return {
    items: [...state.items, assistantItem],
    pendingAssistant: null,
    pendingStartSeq: null,
    counter: state.counter + 1,
  };
}

export const EMPTY_TRANSCRIPT_STATE: ReducerState = {
  items: [],
  pendingAssistant: null,
  pendingStartSeq: null,
  counter: 0,
};

export function buildTranscript(
  events: ReadonlyArray<StreamedEvent>,
): ReadonlyArray<TranscriptItem> {
  let state: ReducerState = EMPTY_TRANSCRIPT_STATE;

  for (const event of events) {
    if (event.type === "stream-reset") {
      state = EMPTY_TRANSCRIPT_STATE;
      continue;
    }

    if (event.type === "session.error") {
      const parsed = parseJson<{ error: string; status: string; message: string }>(event.data);
      state = flushPendingAssistant(state);
      const errorEntry: ErrorTranscriptItem = parsed?.status !== undefined
        ? { kind: "error", id: `e-${state.counter}`, message: parsed.message ?? "Session ended.", code: parsed.status }
        : { kind: "error", id: `e-${state.counter}`, message: parsed?.message ?? "Session ended." };
      state = {
        items: [...state.items, errorEntry],
        pendingAssistant: null,
        pendingStartSeq: null,
        counter: state.counter + 1,
      };
      continue;
    }

    if (event.type !== "session.event") continue;

    const envelope = parseJson<unknown>(event.data);
    if (!isChatSessionEventEnvelope(envelope)) continue;

    const { payload, sequence } = envelope;

    if (payload.type === "turn_complete") {
      state = flushPendingAssistant(state);
      continue;
    }

    if (payload.type === "user_input") {
      state = flushPendingAssistant(state);
      state = {
        items: [
          ...state.items,
          { kind: "user", id: `u-${sequence}`, text: payload.text, seq: sequence },
        ],
        pendingAssistant: [],
        pendingStartSeq: null,
        counter: state.counter + 1,
      };
      continue;
    }

    const pending = state.pendingAssistant ?? [];
    state = {
      ...state,
      pendingAssistant: appendAssistantItem(pending, payload, sequence),
      // Record the sequence of the first event so ChatPage can match against DB seq.
      pendingStartSeq: state.pendingStartSeq ?? sequence,
    };
  }

  // Flush any in-progress assistant turn (streaming).
  if (state.pendingAssistant !== null && state.pendingAssistant.length > 0) {
    return [
      ...state.items,
      {
        kind: "assistant",
        id: `a-${state.counter}`,
        items: state.pendingAssistant,
        inProgress: true,
        ...(state.pendingStartSeq !== null ? { startSeq: state.pendingStartSeq } : {}),
      },
    ];
  }

  return state.items;
}

// Finds the sequence number of the first user_input event in the SSE stream.
// Used to determine the cutoff point when merging historical + live transcripts.
export function getMinSseUserInputSeq(events: ReadonlyArray<StreamedEvent>): number | null {
  for (const event of events) {
    if (event.type !== "session.event") continue;
    const envelope = parseJson<{ sequence: number; payload: { type: string } }>(event.data);
    if (envelope?.payload?.type === "user_input" && typeof envelope.sequence === "number") {
      return envelope.sequence;
    }
  }
  return null;
}

// Try to parse a message body as a JSON content-block array (post-1896 format).
// Returns null if the body is plain text (pre-1896 rows) or invalid JSON.
function tryParseContentBlocks(body: string): ReadonlyArray<AssistantItem> | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(body);
  } catch {
    return null;
  }
  if (!Array.isArray(parsed) || parsed.length === 0) return null;

  const result: AssistantItem[] = [];
  for (let idx = 0; idx < parsed.length; idx++) {
    const block: unknown = parsed[idx];
    if (typeof block !== "object" || block === null) continue;
    const b = block as Record<string, unknown>;

    if (b.type === "text" && typeof b.text === "string") {
      result.push({ type: "text", text: b.text, seq: idx });
    } else if (b.type === "thinking" && typeof b.thinking === "string") {
      result.push({ type: "thinking", thinking: b.thinking, seq: idx });
    } else if (b.type === "tool_use" && typeof b.id === "string" && typeof b.name === "string") {
      result.push({ type: "tool_use", id: b.id, name: b.name, input: b.input, seq: idx });
    } else if (b.type === "tool_result" && typeof b.tool_use_id === "string") {
      result.push({
        type: "tool_result",
        toolUseId: b.tool_use_id as string,
        content: b.content,
        isError: Boolean(b.is_error),
        seq: idx,
      });
    }
  }

  return result.length > 0 ? result : null;
}

export function buildTranscriptFromHistory(
  messages: ReadonlyArray<ChatMessage>,
): ReadonlyArray<TranscriptItem> {
  const items: TranscriptItem[] = [];
  for (const msg of messages) {
    if (msg.role === "user") {
      items.push({ kind: "user", id: msg.id, text: msg.body, seq: msg.seq });
    } else if (msg.role === "assistant") {
      // Try JSON content-block array (post-1896); fall back to plain text for old rows.
      const contentBlocks = tryParseContentBlocks(msg.body);
      items.push({
        kind: "assistant",
        id: msg.id,
        items: contentBlocks ?? [{ type: "text", text: msg.body, seq: msg.seq }],
      });
    }
    // system / tool rows are not rendered in the transcript UI
  }
  return items;
}
