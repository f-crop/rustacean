import { useMemo, useState } from "react";
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

  const setActiveSessionId = (id: string | null) => {
    void navigate({
      to: routes.chat,
      search: id !== null ? { sessionId: id } : {},
      replace: false,
    });
  };

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

    if (!firstLiveUser) {
      // No live user turn in SSE — show all historical messages (reload path).
      return buildTranscriptFromHistory(historical);
    }

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
    return [...buildTranscriptFromHistory(historicalFiltered), ...liveItems];
  }, [historicalMessages.data, events]);

  const isStreaming = sendMessage.isPending;

  const handleNewSession = async (runtime: ChatRuntime) => {
    const result = await createSession.mutateAsync({ runtime });
    setActiveSessionId(result.session_id);
  };

  const handleSend = async (content: string) => {
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
