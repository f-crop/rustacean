// v2 identity-based transcript merge: buildLiveTurnMap + mergeTranscript.

import type { StreamedEvent } from "@/hooks/useEventStream";
import type { ChatMessage } from "@/lib/chat-api";
import type {
  TranscriptItem,
  UserTranscriptItem,
  AssistantTranscriptItem,
  ErrorTranscriptItem,
  AssistantItem,
} from "./transcript";
import {
  parseJson,
  isChatSessionEventEnvelope,
  appendAssistantItem,
  tryParseContentBlocks,
} from "./transcript";

// ---------------------------------------------------------------------------
// v2 identity-based merge: buildLiveTurnMap + mergeTranscript
// ---------------------------------------------------------------------------

/** Concatenate two item arrays, merging a trailing thinking block with a leading thinking block. */
function concatMergingThinking(
  a: ReadonlyArray<AssistantItem>,
  b: ReadonlyArray<AssistantItem>,
): ReadonlyArray<AssistantItem> {
  const lastA = a[a.length - 1];
  const firstB = b[0];
  if (lastA?.type === "thinking" && firstB?.type === "thinking") {
    return [
      ...a.slice(0, -1),
      { type: "thinking", thinking: lastA.thinking + firstB.thinking, seq: lastA.seq },
      ...b.slice(1),
    ];
  }
  return [...a, ...b];
}

/** Concatenate text items from a live turn for content-based dedup. */
function extractLiveText(items: ReadonlyArray<AssistantItem>): string {
  return items
    .filter((i): i is Extract<AssistantItem, { type: "text" }> => i.type === "text")
    .map((i) => i.text)
    .join("");
}

interface LiveTurnEntry {
  readonly items: ReadonlyArray<AssistantItem>;
  readonly isInProgress: boolean;
  readonly firstSeq: number;
  /** User input text for this turn (from SSE user_input event). Used to render
   *  the user bubble when the DB row has not yet been persisted. */
  readonly userText?: string;
}

/** Parse SSE events into a Map<turn_id, LiveTurnEntry>.
 *  Handles both v2 events (with turn_id) and v1 legacy events (no turn_id).
 *  For v1 events a synthetic turn ID is generated to allow accumulation. */
function buildLiveTurnMap(events: ReadonlyArray<StreamedEvent>): {
  turnMap: Map<string, LiveTurnEntry>;
  errorItem: ErrorTranscriptItem | null;
} {
  const turnMap = new Map<string, LiveTurnEntry>();
  let currentTurnId: string | null = null;
  let pendingItems: AssistantItem[] = [];
  let pendingUserText: string | undefined;
  let firstPendingSeq = -1;
  let errorItem: ErrorTranscriptItem | null = null;
  let counter = 0;
  let v1TurnIdx = 0;

  const flush = (inProgress: boolean) => {
    if (currentTurnId !== null && pendingItems.length > 0) {
      const entry: LiveTurnEntry = {
        items: pendingItems,
        isInProgress: inProgress,
        firstSeq: firstPendingSeq,
        ...(pendingUserText !== undefined ? { userText: pendingUserText } : {}),
      };
      turnMap.set(currentTurnId, entry);
    }
    currentTurnId = null;
    pendingItems = [];
    pendingUserText = undefined;
    firstPendingSeq = -1;
  };

  for (const event of events) {
    if (event.type === "stream-reset") {
      turnMap.clear();
      currentTurnId = null;
      pendingItems = [];
      pendingUserText = undefined;
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
      // v2: use the event's turn_id; v1: generate a synthetic turn ID.
      currentTurnId = turnId ?? `v1-live-${v1TurnIdx++}`;
      pendingUserText = typeof payload.text === "string" ? payload.text : undefined;
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
    } else if (turnId === null && currentTurnId === null) {
      // v1 orphan content: no preceding user_input and no turn_id — create a synthetic turn.
      currentTurnId = `v1-orphan-${sequence}`;
    }
    if (currentTurnId !== null) {
      if (firstPendingSeq < 0) firstPendingSeq = sequence;
      pendingItems = appendAssistantItem(pendingItems, payload, sequence, Date.now()) as AssistantItem[];
    }
  }

  // Flush final in-progress turn.
  if (currentTurnId !== null && pendingItems.length > 0) {
    const entry: LiveTurnEntry = {
      items: pendingItems,
      isInProgress: true,
      firstSeq: firstPendingSeq,
      ...(pendingUserText !== undefined ? { userText: pendingUserText } : {}),
    };
    turnMap.set(currentTurnId, entry);
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
  // Tracks turn IDs for which a user row has been rendered from DB.
  // Used to suppress live user bubbles for turns already persisted.
  const processedUserTurnIds = new Set<string>();
  // For v1 DB rows (no turn_id): track seq + text so CLI-replay orphan turns
  // that duplicate already-persisted content can be filtered in the append loop.
  const coveredV1Seqs = new Set<number>();
  const coveredV1Bodies = new Set<string>();

  for (const msg of historical) {
    if (msg.role === "user") {
      result.push({ kind: "user", id: msg.id, text: msg.body, seq: msg.seq });
      if (msg.turn_id) processedUserTurnIds.add(msg.turn_id);
      continue;
    }

    if (msg.role !== "assistant") continue;

    const turnId = msg.turn_id;
    const ts = new Date(msg.created_at).getTime();
    const contentBlocks = tryParseContentBlocks(msg.body, ts);
    const newItems: ReadonlyArray<AssistantItem> =
      contentBlocks ?? [{ type: "text", text: msg.body, seq: msg.seq, ts }];

    if (turnId) {
      processedLiveTurnIds.add(turnId);
      const live = turnMap.get(turnId);

      const existingIdx = assistantIndexByTurnId.get(turnId);
      if (existingIdx !== undefined) {
        // Another DB row for the same turn_id (split-batch): merge content.
        const existing = result[existingIdx] as AssistantTranscriptItem;
        const mergedItems = live?.isInProgress
          ? live.items
          : concatMergingThinking(existing.items, newItems);
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
      coveredV1Seqs.add(msg.seq);
      const bodyText = contentBlocks
        ? contentBlocks
            .filter((b): b is Extract<AssistantItem, { type: "text" }> => b.type === "text")
            .map((b) => b.text)
            .join("")
        : msg.body;
      if (bodyText) coveredV1Bodies.add(bodyText);
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
  // Also emit a user bubble when the SSE user_input arrived before the DB row was persisted,
  // but only if the DB has not already supplied a user row for this turn.
  for (const [turnId, live] of turnMap) {
    if (!processedLiveTurnIds.has(turnId) && live.items.length > 0) {
      // v1 turns (v1-orphan-* and v1-live-*) are synthetic IDs for legacy SSE without turn_id.
      // Filter completed v1 turns that duplicate already-persisted DB content (matched by
      // seq or text), so CLI-replayed and reconnected streams don't double the transcript.
      if (turnId.startsWith("v1-") && !live.isInProgress) {
        if (live.firstSeq !== -1 && coveredV1Seqs.has(live.firstSeq)) continue;
        if (coveredV1Bodies.size > 0 && coveredV1Bodies.has(extractLiveText(live.items))) continue;
      }
      if (live.userText !== undefined && !processedUserTurnIds.has(turnId)) {
        result.push({
          kind: "user",
          id: `u-live-${turnId}`,
          text: live.userText,
          seq: live.firstSeq - 1,
        });
      }
      result.push({
        kind: "assistant",
        id: `a-live-${turnId}`,
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
