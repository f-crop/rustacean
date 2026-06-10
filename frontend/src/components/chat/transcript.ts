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
  | { type: "text"; text: string; seq: number }
  | { type: "thinking"; thinking: string; seq: number }
  | { type: "tool_use"; id: string; name: string; input: unknown; seq: number }
  | { type: "tool_result"; toolUseId: string; content: unknown; isError: boolean; seq: number }
  | { type: "error"; message: string; code?: string; seq: number };

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// v2 identity-based merge: buildLiveTurnMap + mergeTranscript
// ---------------------------------------------------------------------------

interface LiveTurnEntry {
  readonly items: ReadonlyArray<AssistantItem>;
  readonly isInProgress: boolean;
  readonly firstSeq: number;
}

/** Parse SSE events into a Map<turn_id, LiveTurnEntry>.
 *  Returns null entries when turn_id is absent (v1 legacy events are ignored). */
function buildLiveTurnMap(events: ReadonlyArray<StreamedEvent>): {
  turnMap: Map<string, LiveTurnEntry>;
  errorItem: ErrorTranscriptItem | null;
} {
  const turnMap = new Map<string, LiveTurnEntry>();
  let currentTurnId: string | null = null;
  let pendingItems: AssistantItem[] = [];
  let firstPendingSeq = -1;
  let errorItem: ErrorTranscriptItem | null = null;
  let counter = 0;

  const flush = (inProgress: boolean) => {
    if (currentTurnId !== null && pendingItems.length > 0) {
      turnMap.set(currentTurnId, {
        items: pendingItems,
        isInProgress: inProgress,
        firstSeq: firstPendingSeq,
      });
    }
    currentTurnId = null;
    pendingItems = [];
    firstPendingSeq = -1;
  };

  for (const event of events) {
    if (event.type === "stream-reset") {
      turnMap.clear();
      currentTurnId = null;
      pendingItems = [];
      firstPendingSeq = -1;
      errorItem = null;
      continue;
    }

    if (event.type === "session.error") {
      flush(false);
      const parsed = parseJson<{ error: string; status: string; message: string }>(event.data);
      errorItem = parsed?.status !== undefined
        ? { kind: "error", id: `e-${counter++}`, message: parsed.message ?? "Session ended.", code: parsed.status }
        : { kind: "error", id: `e-${counter++}`, message: parsed?.message ?? "Session ended." };
      continue;
    }

    if (event.type !== "session.event") continue;

    const envelope = parseJson<unknown>(event.data);
    if (!isChatSessionEventEnvelope(envelope)) continue;

    const { payload, sequence } = envelope;
    const turnId = (envelope as { turn_id?: string }).turn_id ?? null;

    if (payload.type === "user_input") {
      flush(false);
      currentTurnId = turnId;
      pendingItems = [];
      firstPendingSeq = -1;
      continue;
    }

    if (payload.type === "turn_complete") {
      // tool_use stop_reason = intermediate pause; continue accumulating the same turn.
      if (payload.stop_reason !== "tool_use") {
        flush(false);
      }
      continue;
    }

    // Runtime content event — accumulate into current turn.
    if (turnId !== null && currentTurnId === null) {
      // First event for this turn (no preceding user_input in SSE, e.g. CLI restart).
      currentTurnId = turnId;
    }
    if (currentTurnId !== null) {
      if (firstPendingSeq < 0) firstPendingSeq = sequence;
      pendingItems = appendAssistantItem(pendingItems, payload, sequence) as AssistantItem[];
    }
  }

  // Flush final in-progress turn.
  if (currentTurnId !== null && pendingItems.length > 0) {
    turnMap.set(currentTurnId, {
      items: pendingItems,
      isInProgress: true,
      firstSeq: firstPendingSeq,
    });
  }

  return { turnMap, errorItem };
}

export interface PendingUserSend {
  id: string;
  text: string;
}

/**
 * Identity-based transcript merge (AC-2 of RUSAA-1974).
 *
 * Algorithm:
 *  1. Build live turn map keyed by turn_id from SSE events.
 *  2. Walk historical DB rows in seq order:
 *     - user rows: render directly.
 *     - assistant rows with turn_id: check live map; use live content if in-progress,
 *       else DB content. Merge split-batch rows sharing the same turn_id.
 *     - assistant rows without turn_id (legacy v1): render positionally, merging
 *       consecutive rows that share a tool_use boundary (existing split-batch logic).
 *  3. Append live turns whose turn_id is not yet in the DB (new in-flight turn).
 *  4. Append pending user sends not yet covered by history or SSE echo.
 *  5. Append SSE error item if present.
 */
export function mergeTranscript(
  historical: ReadonlyArray<ChatMessage>,
  liveEvents: ReadonlyArray<StreamedEvent>,
  pendingQueue: ReadonlyArray<PendingUserSend> = [],
): ReadonlyArray<TranscriptItem> {
  const { turnMap, errorItem } = buildLiveTurnMap(liveEvents);

  const result: TranscriptItem[] = [];
  // Tracks the result-array index of each assistant item, keyed by turn_id.
  // Used to merge split-batch rows (multiple DB rows sharing the same turn_id).
  const assistantIndexByTurnId = new Map<string, number>();
  const processedLiveTurnIds = new Set<string>();

  for (const msg of historical) {
    if (msg.role === "user") {
      result.push({ kind: "user", id: msg.id, text: msg.body, seq: msg.seq });
      continue;
    }

    if (msg.role !== "assistant") continue;

    const turnId = msg.turn_id;
    const contentBlocks = tryParseContentBlocks(msg.body);
    const newItems: ReadonlyArray<AssistantItem> =
      contentBlocks ?? [{ type: "text", text: msg.body, seq: msg.seq }];

    if (turnId) {
      processedLiveTurnIds.add(turnId);
      const live = turnMap.get(turnId);

      const existingIdx = assistantIndexByTurnId.get(turnId);
      if (existingIdx !== undefined) {
        // Another DB row for the same turn_id (split-batch): merge content.
        const existing = result[existingIdx] as AssistantTranscriptItem;
        const mergedItems = live?.isInProgress
          ? live.items
          : [...existing.items, ...newItems];
        const updatedItem: AssistantTranscriptItem = {
          kind: "assistant",
          id: existing.id,
          items: mergedItems,
          ...(existing.startSeq !== undefined ? { startSeq: existing.startSeq } : {}),
          ...(live?.isInProgress ? { inProgress: true } : {}),
        };
        result[existingIdx] = updatedItem;
      } else {
        const items = live?.isInProgress ? live.items : newItems;
        const item: AssistantTranscriptItem = {
          kind: "assistant",
          id: msg.id,
          items,
          ...(live?.isInProgress ? { inProgress: true } : {}),
        };
        assistantIndexByTurnId.set(turnId, result.length);
        result.push(item);
      }
    } else {
      // v1 legacy row (no turn_id): positional split-batch merge.
      const prev = result[result.length - 1];
      if (
        prev?.kind === "assistant" &&
        contentBlocks !== null &&
        (prev.items[prev.items.length - 1]?.type === "tool_use" ||
          newItems[0]?.type === "tool_use")
      ) {
        result[result.length - 1] = {
          kind: "assistant",
          id: prev.id,
          items: [...prev.items, ...newItems],
          ...(prev.startSeq !== undefined ? { startSeq: prev.startSeq } : {}),
        };
      } else {
        result.push({ kind: "assistant", id: msg.id, items: newItems });
      }
    }
  }

  // Append live turns not yet present in the DB.
  for (const [turnId, live] of turnMap) {
    if (!processedLiveTurnIds.has(turnId) && live.items.length > 0) {
      result.push({
        kind: "assistant",
        id: `a-live-${turnId.slice(0, 8)}`,
        items: live.items,
        ...(live.isInProgress ? { inProgress: true } : {}),
      });
    }
  }

  // Append SSE error item.
  if (errorItem !== null) {
    result.push(errorItem);
  }

  // Collect all user texts already rendered so pending sends can be filtered.
  const coveredTexts = new Set<string>();
  for (const item of result) {
    if (item.kind === "user") coveredTexts.add(item.text);
  }
  // Also cover from SSE user_input echoes (may arrive before DB persists the row).
  for (const event of liveEvents) {
    if (event.type !== "session.event") continue;
    const env = parseJson<{ payload: { type: string; text?: string } }>(event.data);
    if (env?.payload?.type === "user_input" && typeof env.payload.text === "string") {
      coveredTexts.add(env.payload.text);
    }
  }

  const pendingItems: UserTranscriptItem[] = pendingQueue
    .filter((p) => !coveredTexts.has(p.text))
    .map((p, i) => ({
      kind: "user" as const,
      id: p.id,
      text: p.text,
      seq: -(i + 1),
    }));

  return [...result, ...pendingItems];
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
      const contentBlocks = tryParseContentBlocks(msg.body);
      const newItems: ReadonlyArray<AssistantItem> =
        contentBlocks ?? [{ type: "text", text: msg.body, seq: msg.seq }];

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
