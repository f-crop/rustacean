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

    let base: ReadonlyArray<TranscriptItem>;

    if (!firstLiveUser) {
      const histItems = buildTranscriptFromHistory(historical);
      // SSE has no user_input events; liveItems holds only assistant turns.
      // Deduplicate by matching each live assistant's startSeq against the DB
      // seq values of persisted assistant messages. This correctly handles both
      // the 4-turn replay case (RUSAA-1934) and the normal reconnect case where
      // only the current in-progress turn streams (no false positives from count).
      const histAssistantSeqs = new Set<number>(
        historical.filter((m) => m.role === "assistant").map((m) => m.seq),
      );
      const extraLive = liveItems.filter((item) => {
        if (item.kind !== "assistant") return true;
        const { startSeq } = item;
        return startSeq === undefined || !histAssistantSeqs.has(startSeq);
      });
      base = [...histItems, ...extraLive];
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
      base = [...buildTranscriptFromHistory(historicalFiltered), ...liveItems];
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
