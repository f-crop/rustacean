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
  mergeTranscript,
  type PendingUserSend,
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
  // Optimistic user bubbles: pushed on send, filtered out once SSE echoes or DB persists.
  const [pendingUserSends, setPendingUserSends] = useState<ReadonlyArray<PendingUserSend>>([]);
  // Messages typed while the assistant is streaming — drained FIFO on turn_complete.
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

  const { events, readyState, isStreaming } = useChatStream(activeSessionId);

  const transcript = useMemo(
    () =>
      mergeTranscript(
        historicalMessages.data?.messages ?? [],
        events,
        pendingUserSends,
      ),
    [historicalMessages.data, events, pendingUserSends],
  );

  // AC-1: isComposerLocked = isStreaming (SSE) ∨ queuedSends pending ∨ session creating.
  // sendMessage.isPending is intentionally excluded — POST resolves in ~200ms while
  // the assistant streams for 5–60s; locking on isPending re-enables the composer too early.
  const isStreamingOrBusy = isStreaming || createSession.isPending;
  const isComposerLocked = isStreamingOrBusy || queuedSends.length > 0;

  const handleNewSession = async (runtime: ChatRuntime) => {
    const result = await createSession.mutateAsync({ runtime });
    setActiveSessionId(result.session_id);
  };

  // Stable ref so the drain effect always calls the latest handleSend without
  // adding it to the effect dependency array (which would re-fire on every render).
  const handleSendRef = useRef<(content: string) => Promise<void>>(async () => {});

  const handleSend = async (content: string) => {
    // Queue if the assistant is still streaming or a session is being created.
    if (isStreamingOrBusy) {
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

  // Drain queued sends head-first when no longer streaming.
  // Guard on isStreamingOrBusy (not isComposerLocked) to avoid the circularity where
  // queuedSends.length > 0 keeps isComposerLocked true and the drain never fires.
  useEffect(() => {
    if (isStreamingOrBusy || queuedSends.length === 0) return;
    const [next, ...rest] = queuedSends;
    setQueuedSends(rest);
    void handleSendRef.current(next!);
  }, [isStreamingOrBusy, queuedSends]);

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
              isQueuing={isStreaming}
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
