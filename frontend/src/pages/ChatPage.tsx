import { useEffect, useMemo, useState } from "react";
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
  type AssistantTranscriptItem,
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
  // echoes user_input or the DB history reflects the message (Bug A fix).
  const [pendingUserSends, setPendingUserSends] = useState<
    ReadonlyArray<{ id: string; text: string }>
  >([]);

  const setActiveSessionId = (id: string | null) => {
    void navigate({
      to: routes.chat,
      search: id !== null ? { sessionId: id } : {},
      replace: false,
    });
  };

  // Clear pending sends when the user navigates to a different session.
  useEffect(() => {
    setPendingUserSends([]);
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
      // No live user turn in SSE — show historical + any live non-user items (e.g. session.error).
      base = [...buildTranscriptFromHistory(historical), ...liveItems];
    } else {
      // The SSE stream is covering at least one user turn.  Find the last historical
      // user message whose body matches the first SSE user turn and exclude it (and
      // all subsequent rows) from the historical slice — those rows will be supplied
      // by the live stream instead, preventing duplication.
      //
      // If the matching message is not yet in the DB cache (common: messages query
      // has a 30 s staleTime and POST /messages doesn't invalidate it), cutIdx stays
      // -1 and all historical messages are shown before the live items.
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

    // Collect user texts already present in live SSE or historical so we don't
    // duplicate a pending send that has already been echoed.
    const coveredTexts = new Set<string>();
    for (const item of liveItems) {
      if (item.kind === "user") coveredTexts.add(item.text);
    }
    for (const msg of historical) {
      if (msg.role === "user") coveredTexts.add(msg.body);
    }

    const pendingItems: UserTranscriptItem[] = pendingUserSends
      .filter((p) => !coveredTexts.has(p.text))
      .map((p, i) => ({
        kind: "user" as const,
        id: p.id,
        text: p.text,
        seq: -(i + 1),
      }));

    if (pendingItems.length === 0) return base;

    // Slot pending bubbles BEFORE any trailing in-progress assistant turn from the
    // live stream.  Without this, the pending bubble appends after a streaming
    // assistant response, reversing chronological order during the SSE echo race
    // window (between POST completing and user_input echo arriving).
    //
    // Guard: only slot when the live SSE stream has NO user_input echo yet.
    // If liveItems already contains a user_input event, the in-progress assistant
    // is the stale-inProgress completed response from the prior turn (inProgress is
    // only cleared when a subsequent user_input flushes pendingAssistant).
    // Slotting before it would misplace turn-2's pending bubble between user-1 and
    // the completed assistant-1.
    const firstInProgressIdx = liveItems.findIndex(
      (item): item is AssistantTranscriptItem =>
        item.kind === "assistant" && item.inProgress === true,
    );
    const liveHasUserEcho = liveItems.some((item) => item.kind === "user");

    let insertAt = base.length;
    if (firstInProgressIdx !== -1 && !liveHasUserEcho) {
      const candidateSlot = base.length - liveItems.length + firstInProgressIdx;
      // Secondary guard: if the item immediately before the candidate slot is a
      // user turn, the in-progress assistant is already paired with that user
      // message (sourced from DB history when SSE missed the user_input event).
      // Inserting here would wedge the pending bubble between the historical
      // user turn and its response — the same inversion we're preventing.
      if (base[candidateSlot - 1]?.kind !== "user") {
        insertAt = candidateSlot;
      }
    }

    return [
      ...base.slice(0, insertAt),
      ...pendingItems,
      ...base.slice(insertAt),
    ];
  }, [historicalMessages.data, events, pendingUserSends]);

  const isStreaming = sendMessage.isPending;

  const handleNewSession = async (runtime: ChatRuntime) => {
    const result = await createSession.mutateAsync({ runtime });
    setActiveSessionId(result.session_id);
  };

  const handleSend = async (content: string) => {
    if (activeSessionId) {
      // Optimistic: show user bubble immediately before the SSE user_input echo arrives.
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
            <MessageThread items={transcript} isStreaming={isStreaming} />
            <MessageComposer
              value={composerValue}
              onChange={setComposerValue}
              onSend={(content) => {
                void handleSend(content);
              }}
              isDisabled={isStreaming || createSession.isPending}
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
