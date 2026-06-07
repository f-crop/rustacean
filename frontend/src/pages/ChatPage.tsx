import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { useMe } from "@/api";
import {
  useChatSessions,
  useCreateChatSession,
  useSendChatMessage,
  useChatMessages,
} from "@/api/hooks/useChatSessions";
import { useChatStream } from "@/hooks/useChatStream";
import { SessionSidebar } from "@/components/chat/SessionSidebar";
import { MessageThread } from "@/components/chat/MessageThread";
import { MessageComposer } from "@/components/chat/MessageComposer";
import {
  buildTranscript,
  buildTranscriptFromHistory,
  type TranscriptItem,
  type UserTranscriptItem,
} from "@/components/chat/transcript";
import { formatApiError } from "@/lib/errors/api";
import { routes } from "@/lib/routes";
import type { ChatRuntime } from "@/lib/chat-api";

export function ChatPage(): JSX.Element {
  const me = useMe({ retry: false });

  if (me.isLoading) {
    return (
      <div className="flex h-[calc(100vh-3.5rem-4rem)] items-center justify-center">
        <p className="text-sm text-muted-foreground">Loading…</p>
      </div>
    );
  }

  if (me.isError || !me.data) {
    return (
      <div className="flex h-[calc(100vh-3.5rem-4rem)] items-center justify-center">
        <p className="text-sm text-muted-foreground">Sign in to use Chat.</p>
      </div>
    );
  }

  return <ChatInner tenantId={me.data.current_tenant.id} />;
}

interface ChatInnerProps {
  readonly tenantId: string;
}

// Deduplicate assistant items within user-input segments.
//
// Per segment [user, ...assistants...]:
//   - In-progress present: keep only the last in-progress; discard completed replays.
//   - 2+ completed, no in-progress: CLI always replays ALL prior turns first; fresh
//     answers (if any) follow. Drop the first min(histAssistantSeqs.size, count) items
//     (replays); keep any beyond that count. Without histAssistantSeqs, drop all.
//   - 0 or 1 completed: keep as-is.
function dedupeAssistantsPerSegment(
  items: ReadonlyArray<TranscriptItem>,
  histAssistantSeqs?: ReadonlySet<number>,
): ReadonlyArray<TranscriptItem> {
  const result: TranscriptItem[] = [];
  let i = 0;
  while (i < items.length) {
    const item = items[i];
    if (!item) { i++; continue; }
    if (item.kind !== "user") {
      result.push(item);
      i++;
      continue;
    }
    // Collect this user-input and everything until the next user-input.
    result.push(item);
    i++;
    const segStart = i;
    while (i < items.length && items[i]?.kind !== "user") {
      i++;
    }
    const segment = items.slice(segStart, i);

    // Find the last in-progress assistant (the actual streaming response).
    let lastInProgressIdx = -1;
    for (let k = segment.length - 1; k >= 0; k--) {
      const s = segment[k];
      if (s?.kind === "assistant" && s.inProgress === true) {
        lastInProgressIdx = k;
        break;
      }
    }

    if (lastInProgressIdx >= 0) {
      // Streaming response present: keep only it, discard all completed replays.
      for (let k = 0; k < segment.length; k++) {
        const seg = segment[k];
        if (!seg) continue;
        if (seg.kind === "assistant" && k !== lastInProgressIdx) continue;
        result.push(seg);
      }
    } else {
      // No streaming response. Count completed assistants in this segment.
      const completedCount = segment.filter((s) => s?.kind === "assistant").length;
      if (completedCount >= 2) {
        // 2+ completed, no in-progress. CLI replays all prior turns first; the fresh
        // answer (if it has arrived) follows. Drop the first replayCount completed
        // items; keep the remainder.
        const replayCount = histAssistantSeqs
          ? Math.min(histAssistantSeqs.size, completedCount)
          : completedCount;
        const completedItems = segment.filter((s) => s?.kind === "assistant");
        const freshSet = new Set(completedItems.slice(replayCount));
        for (const seg of segment) {
          if (!seg) continue;
          if (seg.kind === "assistant" && !freshSet.has(seg)) continue;
          result.push(seg);
        }
      } else {
        // 0 or 1 completed: real historical answer or no response yet; keep as-is.
        for (const seg of segment) {
          if (seg) result.push(seg);
        }
      }
    }
  }
  return result;
}

function ChatInner({ tenantId }: ChatInnerProps): JSX.Element {
  const navigate = useNavigate();
  const { sessionId: activeSessionId = null } = useSearch({ from: routes.chat });
  const [composerValue, setComposerValue] = useState("");
  // Optimistic user bubbles: entries pushed immediately on send, removed once SSE
  // echoes user_input or the DB history reflects the message.
  const [pendingUserSends, setPendingUserSends] = useState<
    ReadonlyArray<{ id: string; text: string }>
  >([]);
  // Messages typed while the assistant is streaming — drained in order after completion.
  const [queuedSends, setQueuedSends] = useState<ReadonlyArray<string>>([]);

  const setActiveSessionId = (id: string | null) => {
    void navigate({
      to: routes.chat,
      search: id !== null ? { sessionId: id } : {},
      replace: false,
    });
  };

  // Clear optimistic + queued state when navigating to a different session.
  useEffect(() => {
    setPendingUserSends([]);
    setQueuedSends([]);
  }, [activeSessionId]);

  const sessions = useChatSessions(tenantId);
  const createSession = useCreateChatSession(tenantId);
  const sendMessage = useSendChatMessage();
  const historicalMessages = useChatMessages(activeSessionId);

  const { events, readyState } = useChatStream(activeSessionId);

  const transcript = useMemo(() => {
    const historical = historicalMessages.data?.messages ?? [];
    const liveItems = buildTranscript(events);

    // Find the first user turn emitted by the live SSE stream.
    const firstLiveUser = liveItems.find(
      (item): item is UserTranscriptItem => item.kind === "user",
    );

    // DB seq values of persisted assistant messages. Used by dedupeAssistantsPerSegment
    // in both merge paths to distinguish CLI-replayed prior-turn responses (startSeq
    // already in DB) from fresh completions (startSeq not yet written to DB).
    const histAssistantSeqs = new Set<number>(
      historical.filter((m) => m.role === "assistant").map((m) => m.seq),
    );

    let base: ReadonlyArray<TranscriptItem>;

    if (!firstLiveUser) {
      const histItems = buildTranscriptFromHistory(historical);
      // SSE has no user_input events; liveItems holds only assistant turns.
      // Deduplicate by matching each live assistant's startSeq against the DB
      // seq values of persisted assistant messages. Always keep the in-progress
      // (streaming) assistant regardless of startSeq — its sequence may collide
      // with a persisted assistant when all turns use the same batch slot (e.g.
      // simple single-text-block turns where Text is always at position 2).
      const hasLiveInProgress = liveItems.some(
        (item) => item.kind === "assistant" && item.inProgress === true,
      );

      // Build a set of historical assistant text bodies for content-based replay
      // detection. When the CLI restarts and assigns NEW sequence numbers to
      // replayed responses (not reusing original DB seqs), the seq-based filter
      // below cannot distinguish replays from the fresh answer. As a fallback,
      // if a live completed assistant's full text matches any historical
      // assistant's body verbatim it is a replay and must be dropped.
      // This handles the turn-2 bug (R27): fresh arrives first in SSE (lower
      // startSeq), prior-turn replay arrives second — content matching identifies
      // which is which regardless of SSE ordering.
      const histAssistantTexts = new Set<string>();
      for (const histItem of histItems) {
        if (histItem.kind !== "assistant") continue;
        const text = histItem.items
          .filter((i) => i.type === "text")
          .map((i) => (i.type === "text" ? i.text : ""))
          .join("");
        if (text) histAssistantTexts.add(text);
      }

      const extraLive = liveItems.filter((item) => {
        if (item.kind !== "assistant") return true;
        if (item.inProgress === true) return true;
        // When the live stream contains an in-progress response, all completed
        // assistants are CLI-replayed prior-turn responses — drop them.
        if (hasLiveInProgress) return false;
        // Drop replays identified by seq (same-seq CLI restart: CLI reuses the
        // original DB sequence numbers when replaying prior-turn responses).
        const { startSeq } = item;
        if (startSeq !== undefined && histAssistantSeqs.has(startSeq)) return false;
        // Drop replays identified by content (new-seq CLI restart: CLI assigns
        // new sequence numbers to replayed responses, so seq matching fails).
        // If the live assistant's text matches any historical assistant verbatim,
        // it is a prior-turn replay regardless of sequence number.
        const liveText = item.items
          .filter((i) => i.type === "text")
          .map((i) => (i.type === "text" ? i.text : ""))
          .join("");
        if (liveText && histAssistantTexts.has(liveText)) return false;
        return true;
      });
      // Deduplicate: per-segment safety net in case a replay slips past both
      // the seq and content filters (e.g. new seqs AND novel content — edge
      // case). histAssistantSeqs bounds the replay count in those cases.
      base = dedupeAssistantsPerSegment([...histItems, ...extraLive], histAssistantSeqs);
    } else {
      // Exclude historical rows that are covered by the live stream to prevent duplication.
      let cutIdx = -1;
      for (let i = historical.length - 1; i >= 0; i--) {
        const msg = historical[i];
        if (msg && msg.role === "user" && msg.body === firstLiveUser.text) {
          cutIdx = i;
          break;
        }
      }

      const historicalFiltered = cutIdx >= 0 ? historical.slice(0, cutIdx) : historical;
      // Deduplicate assistant items within each user-input segment of liveItems.
      // When the CLI restarts mid-session it replays all prior-turn responses within
      // the current turn's segment. Pass histAssistantSeqs so only replay assistants
      // (startSeq already in DB) are dropped; the real just-completed answer
      // (startSeq not yet in DB) is preserved.
      base = [...buildTranscriptFromHistory(historicalFiltered), ...dedupeAssistantsPerSegment(liveItems, histAssistantSeqs)];
    }

    if (pendingUserSends.length === 0) return base;

    // Collect user texts already present so we don't duplicate an echoed pending send.
    const coveredTexts = new Set<string>();
    for (const item of liveItems) {
      if (item.kind === "user") coveredTexts.add(item.text);
    }
    for (const msg of historical) {
      if (msg.role === "user") coveredTexts.add(msg.body);
    }

    // F-3: With the queue gate in place, pendingUserSends.length is provably ≤ 1
    // (one in-flight message paired with the streaming assistant). The complex slot
    // heuristic from previous PRs is replaced with a simple append of uncovered items.
    const pendingItems: UserTranscriptItem[] = pendingUserSends
      .filter((p) => !coveredTexts.has(p.text))
      .map((p, i) => ({
        kind: "user" as const,
        id: p.id,
        text: p.text,
        seq: -(i + 1),
      }));

    return [...base, ...pendingItems];
  }, [historicalMessages.data, events, pendingUserSends]);

  // F-1: Gate on assistant-stream completion, not POST completion.
  // sendMessage.isPending clears ~200 ms after POST; the assistant streams for 5–60 s.
  const assistantStreaming = transcript.some(
    (item) => item.kind === "assistant" && item.inProgress === true,
  );
  const isComposerLocked =
    assistantStreaming || sendMessage.isPending || createSession.isPending;

  const handleNewSession = async (runtime: ChatRuntime) => {
    const result = await createSession.mutateAsync({ runtime });
    setActiveSessionId(result.session_id);
  };

  // Stable ref so the drain effect always calls the latest handleSend without
  // adding it to the effect dependency array (which would re-fire on every render).
  const handleSendRef = useRef<(content: string) => Promise<void>>(async () => {});

  const handleSend = async (content: string) => {
    // F-2: Queue if the assistant is still streaming or a send/session is in flight.
    if (isComposerLocked) {
      setQueuedSends((prev) => [...prev, content]);
      setComposerValue("");
      return;
    }
    if (activeSessionId) {
      const pendingId = `p-${Date.now().toString()}`;
      setPendingUserSends((prev) => [...prev, { id: pendingId, text: content }]);
    }
    if (!activeSessionId) {
      const result = await createSession.mutateAsync({ runtime: "claude_code" });
      setActiveSessionId(result.session_id);
      await sendMessage.mutateAsync({ sessionId: result.session_id, content });
      return;
    }
    await sendMessage.mutateAsync({ sessionId: activeSessionId, content });
  };

  handleSendRef.current = handleSend;

  // F-2: Drain the queue head-first when the composer is fully unlocked.
  // Guard on isComposerLocked (not assistantStreaming alone) so that the effect
  // does not re-fire while sendMessage.isPending is still true — otherwise
  // handleSend queues the next item at the tail instead of sending it, inverting
  // FIFO order across multi-message drains.
  useEffect(() => {
    if (isComposerLocked || queuedSends.length === 0) return;
    const [next, ...rest] = queuedSends;
    setQueuedSends(rest);
    void handleSendRef.current(next!);
  }, [isComposerLocked, queuedSends]);

  const sessionList = sessions.data?.sessions ?? [];

  return (
    <div className="flex h-[calc(100vh-3.5rem-4rem)]">
      <SessionSidebar
        sessions={sessionList}
        activeSessionId={activeSessionId}
        isLoading={sessions.isLoading}
        isError={sessions.isError}
        error={sessions.error}
        isCreating={createSession.isPending}
        onSelectSession={(id) => {
          setActiveSessionId(id);
          setComposerValue("");
        }}
        onNewSession={handleNewSession}
      />

      <div className="flex flex-1 flex-col overflow-hidden">
        <ChatHeader
          sessionId={activeSessionId}
          readyState={readyState}
        />

        {sendMessage.isError && (
          <div
            role="alert"
            className="border-b border-destructive/30 bg-destructive/5 px-4 py-2 text-sm text-destructive"
          >
            {formatApiError(sendMessage.error, "Failed to send message.")}
          </div>
        )}

        {activeSessionId === null ? (
          <div className="flex flex-1 items-center justify-center">
            <div className="text-center">
              <p className="text-sm text-muted-foreground">
                Select a session from the sidebar or start a new one.
              </p>
              <button
                type="button"
                onClick={() => void handleNewSession("claude_code")}
                disabled={createSession.isPending}
                className="mt-3 rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
              >
                {createSession.isPending ? "Starting…" : "New chat session"}
              </button>
            </div>
          </div>
        ) : (
          <>
            <MessageThread items={transcript} isStreaming={isComposerLocked} />
            <MessageComposer
              value={composerValue}
              onChange={setComposerValue}
              onSend={(content) => {
                void handleSend(content);
              }}
              isDisabled={createSession.isPending}
              isQueuing={assistantStreaming || sendMessage.isPending}
              queuedMessages={queuedSends}
            />
          </>
        )}
      </div>
    </div>
  );
}

interface ChatHeaderProps {
  readonly sessionId: string | null;
  readonly readyState: "connecting" | "open" | "closed";
}

function ChatHeader({ sessionId, readyState }: ChatHeaderProps): JSX.Element {
  const connectionColor =
    readyState === "open"
      ? "bg-green-500"
      : readyState === "connecting"
        ? "bg-yellow-500 animate-pulse"
        : "bg-muted-foreground";

  const connectionLabel =
    readyState === "open" ? "Live" : readyState === "connecting" ? "Connecting…" : "";

  return (
    <div className="flex shrink-0 items-center justify-between border-b border-border bg-background/80 px-4 py-2 backdrop-blur">
      <h1 className="text-base font-semibold">Chat</h1>
      <div className="flex items-center gap-3">
        {sessionId && (
          <span className="font-mono text-xs text-muted-foreground" title={sessionId}>
            {sessionId.slice(0, 8)}…
          </span>
        )}
        {sessionId && (
          <span className="flex items-center gap-1 text-xs text-muted-foreground" aria-live="polite">
            <span aria-hidden="true" className={`h-1.5 w-1.5 rounded-full ${connectionColor}`} />
            {connectionLabel}
          </span>
        )}
      </div>
    </div>
  );
}
