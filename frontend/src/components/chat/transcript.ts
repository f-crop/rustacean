// Transcript types and legacy v1 reducer for building a conversational view from SSE events.
// v2 identity-based merge (mergeTranscript) lives in ./merge-transcript.ts.

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
  // True only on the trailing pending turn that has not yet received turn_complete.
  inProgress?: boolean;
  // Sequence number of the first SSE event for this assistant turn. Used by
  // legacy v1 merge path in ChatPage to deduplicate live items against history.
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
  | { type: "text"; text: string; seq: number; ts?: number }
  | { type: "thinking"; thinking: string; seq: number; ts?: number }
  | { type: "tool_use"; id: string; name: string; input: unknown; seq: number; ts?: number }
  | { type: "tool_result"; toolUseId: string; content: unknown; isError: boolean; seq: number; ts?: number }
  | { type: "error"; message: string; code?: string; seq: number; ts?: number };

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

export function parseJson<T>(s: string): T | null {
  try {
    return JSON.parse(s) as T;
  } catch {
    return null;
  }
}

export function isChatSessionEventEnvelope(v: unknown): v is ChatSessionEventEnvelope {
  if (typeof v !== "object" || v === null) return false;
  const o = v as Record<string, unknown>;
  return (
    typeof o.sequence === "number" &&
    typeof o.payload === "object" &&
    o.payload !== null &&
    typeof (o.payload as Record<string, unknown>).type === "string"
  );
}

export function appendAssistantItem(
  pending: ReadonlyArray<AssistantItem>,
  payload: ChatRuntimePayload,
  sequence: number,
  ts?: number,
): ReadonlyArray<AssistantItem> {
  const tsField = ts !== undefined ? { ts } : {};
  if (payload.type === "text") {
    const last = pending[pending.length - 1];
    if (last?.type === "text") {
      return [
        ...pending.slice(0, -1),
        { type: "text", text: last.text + payload.text, seq: last.seq, ...(last.ts !== undefined ? { ts: last.ts } : {}) },
      ];
    }
    return [...pending, { type: "text", text: payload.text, seq: sequence, ...tsField }];
  }
  if (payload.type === "thinking") {
    // Guard: server may omit `thinking` or use a different field; skip the item rather than storing undefined.
    if (typeof payload.thinking !== "string") return pending;
    const lastThinking = pending[pending.length - 1];
    if (lastThinking?.type === "thinking") {
      return [
        ...pending.slice(0, -1),
        { type: "thinking", thinking: lastThinking.thinking + payload.thinking, seq: lastThinking.seq, ...(lastThinking.ts !== undefined ? { ts: lastThinking.ts } : {}) },
      ];
    }
    return [...pending, { type: "thinking", thinking: payload.thinking, seq: sequence, ...tsField }];
  }
  if (payload.type === "tool_use") {
    return [
      ...pending,
      { type: "tool_use", id: payload.id, name: payload.name, input: payload.input, seq: sequence, ...tsField },
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
        ...tsField,
      },
    ];
  }
  if (payload.type === "error") {
    const errorItem: AssistantItem = payload.code !== undefined
      ? { type: "error", message: payload.message, code: payload.code, seq: sequence, ...tsField }
      : { type: "error", message: payload.message, seq: sequence, ...tsField };
    return [...pending, errorItem];
  }
  return pending;
}

// Try to parse a message body as a JSON content-block array (post-1896 format).
// Returns null if the body is plain text (pre-1896 rows) or invalid JSON.
export function tryParseContentBlocks(body: string, ts?: number): ReadonlyArray<AssistantItem> | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(body);
  } catch {
    return null;
  }
  if (!Array.isArray(parsed) || parsed.length === 0) return null;

  const tsField = ts !== undefined ? { ts } : {};
  const result: AssistantItem[] = [];
  for (let idx = 0; idx < parsed.length; idx++) {
    const block: unknown = parsed[idx];
    if (typeof block !== "object" || block === null) continue;
    const b = block as Record<string, unknown>;

    if (b.type === "text" && typeof b.text === "string") {
      result.push({ type: "text", text: b.text, seq: idx, ...tsField });
    } else if (b.type === "thinking" && typeof b.thinking === "string") {
      const lastThinking = result[result.length - 1];
      if (lastThinking?.type === "thinking") {
        result[result.length - 1] = { type: "thinking", thinking: lastThinking.thinking + b.thinking, seq: lastThinking.seq, ...(lastThinking.ts !== undefined ? { ts: lastThinking.ts } : {}) };
      } else {
        result.push({ type: "thinking", thinking: b.thinking, seq: idx, ...tsField });
      }
    } else if (b.type === "tool_use" && typeof b.id === "string" && typeof b.name === "string") {
      result.push({ type: "tool_use", id: b.id, name: b.name, input: b.input, seq: idx, ...tsField });
    } else if (b.type === "tool_result" && typeof b.tool_use_id === "string") {
      result.push({
        type: "tool_result",
        toolUseId: b.tool_use_id as string,
        content: b.content,
        isError: Boolean(b.is_error),
        seq: idx,
        ...tsField,
      });
    }
  }

  return result.length > 0 ? result : null;
}

// ---------------------------------------------------------------------------
// Legacy v1 path — kept for backward compat and existing unit tests.
// Used by transcript.test.ts; not used by ChatPage anymore.
// ---------------------------------------------------------------------------

interface ReducerState {
  readonly items: ReadonlyArray<TranscriptItem>;
  readonly pendingAssistant: ReadonlyArray<AssistantItem> | null;
  readonly pendingStartSeq: number | null;
  readonly counter: number;
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

const EMPTY_TRANSCRIPT_STATE: ReducerState = {
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
      if (payload.stop_reason !== "tool_use") {
        state = flushPendingAssistant(state);
      }
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
      pendingAssistant: appendAssistantItem(pending, payload, sequence) as AssistantItem[],
      pendingStartSeq: state.pendingStartSeq ?? sequence,
    };
  }

  const raw: ReadonlyArray<TranscriptItem> =
    state.pendingAssistant !== null && state.pendingAssistant.length > 0
      ? [
          ...state.items,
          {
            kind: "assistant",
            id: `a-${state.counter}`,
            items: state.pendingAssistant,
            inProgress: true,
            ...(state.pendingStartSeq !== null ? { startSeq: state.pendingStartSeq } : {}),
          },
        ]
      : state.items;

  const hasUserItems = raw.some((item) => item.kind === "user");
  return hasUserItems ? mergeAdjacentToolUseAssistants(raw) : raw;
}

function mergeAdjacentToolUseAssistants(
  items: ReadonlyArray<TranscriptItem>,
): ReadonlyArray<TranscriptItem> {
  const result: TranscriptItem[] = [];
  for (const item of items) {
    const prev = result[result.length - 1];
    if (
      prev !== undefined &&
      prev.kind === "assistant" &&
      !prev.inProgress &&
      item.kind === "assistant" &&
      item.items[0]?.type === "tool_use"
    ) {
      result[result.length - 1] = {
        kind: "assistant",
        id: prev.id,
        items: [...prev.items, ...item.items],
        ...(prev.startSeq !== undefined ? { startSeq: prev.startSeq } : {}),
        ...(item.inProgress === true ? { inProgress: true } : {}),
      };
    } else {
      result.push(item);
    }
  }
  return result;
}

export function buildTranscriptFromHistory(
  messages: ReadonlyArray<ChatMessage>,
): ReadonlyArray<TranscriptItem> {
  const items: TranscriptItem[] = [];
  for (const msg of messages) {
    if (msg.role === "user") {
      items.push({ kind: "user", id: msg.id, text: msg.body, seq: msg.seq });
    } else if (msg.role === "assistant") {
      const ts = new Date(msg.created_at).getTime();
      const contentBlocks = tryParseContentBlocks(msg.body, ts);
      const newItems: ReadonlyArray<AssistantItem> =
        contentBlocks ?? [{ type: "text", text: msg.body, seq: msg.seq, ts }];

      const prev = items[items.length - 1];
      if (
        prev?.kind === "assistant" &&
        contentBlocks !== null &&
        (prev.items[prev.items.length - 1]?.type === "tool_use" ||
          contentBlocks[0]?.type === "tool_use")
      ) {
        items[items.length - 1] = {
          kind: "assistant",
          id: prev.id,
          items: [...prev.items, ...newItems],
          ...(prev.startSeq !== undefined ? { startSeq: prev.startSeq } : {}),
        };
      } else {
        items.push({ kind: "assistant", id: msg.id, items: newItems });
      }
    }
  }
  return items;
}
