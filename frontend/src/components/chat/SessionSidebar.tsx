import type { ChatSession, ChatRuntime } from "@/lib/chat-api";
import type { ApiError } from "@/api/client";
import { formatApiError } from "@/lib/errors/api";
import { formatTimestamp } from "@/components/activity/utils";

interface SessionSidebarProps {
  readonly sessions: ReadonlyArray<ChatSession>;
  readonly activeSessionId: string | null;
  readonly isLoading: boolean;
  readonly isError: boolean;
  readonly error: ApiError | null;
  readonly isCreating: boolean;
  readonly onSelectSession: (id: string) => void;
  readonly onNewSession: (runtime: ChatRuntime) => void;
}

const RUNTIME_LABELS: Record<ChatRuntime, string> = {
  claude_code: "Claude Code",
  opencode: "OpenCode",
  pi: "Pi",
};

const STATUS_DOT: Record<string, string> = {
  active: "bg-green-500",
  ended: "bg-muted-foreground",
  failed: "bg-destructive",
};

export function SessionSidebar({
  sessions,
  activeSessionId,
  isLoading,
  isError,
  error,
  isCreating,
  onSelectSession,
  onNewSession,
}: SessionSidebarProps): JSX.Element {
  return (
    <aside
      aria-label="Chat sessions"
      className="flex w-64 shrink-0 flex-col border-r border-border bg-card"
    >
      <div className="flex items-center justify-between border-b border-border px-4 py-3">
        <h2 className="text-sm font-semibold">Sessions</h2>
        <button
          type="button"
          onClick={() => onNewSession("claude_code")}
          disabled={isCreating}
          aria-label="New chat session"
          className="rounded-md bg-primary px-2 py-1 text-xs font-medium text-primary-foreground hover:bg-primary/90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
        >
          {isCreating ? "Creating…" : "+ New"}
        </button>
      </div>

      <div className="flex-1 overflow-y-auto">
        {isLoading ? (
          <p className="px-4 py-3 text-xs text-muted-foreground">Loading…</p>
        ) : isError ? (
          <p className="px-4 py-3 text-xs text-destructive">
            {formatApiError(error, "Could not load sessions.")}
          </p>
        ) : sessions.length === 0 ? (
          <p className="px-4 py-4 text-xs text-muted-foreground">
            No sessions yet. Click + New to start.
          </p>
        ) : (
          <ul role="list" className="space-y-px py-1">
            {sessions.map((session) => (
              <SessionRow
                key={session.id}
                session={session}
                isActive={session.id === activeSessionId}
                onClick={() => onSelectSession(session.id)}
              />
            ))}
          </ul>
        )}
      </div>
    </aside>
  );
}

interface SessionRowProps {
  readonly session: ChatSession;
  readonly isActive: boolean;
  readonly onClick: () => void;
}

function SessionRow({ session, isActive, onClick }: SessionRowProps): JSX.Element {
  const statusDot = STATUS_DOT[session.status] ?? "bg-muted-foreground";
  const runtimeLabel = RUNTIME_LABELS[session.runtime] ?? session.runtime;
  const created = formatTimestamp(session.created_at);

  return (
    <li>
      <button
        type="button"
        onClick={onClick}
        aria-pressed={isActive}
        className={`w-full px-4 py-2.5 text-left transition-colors hover:bg-accent ${
          isActive ? "bg-accent text-foreground" : "text-muted-foreground"
        }`}
      >
        <div className="flex items-center gap-2">
          <span
            aria-hidden="true"
            className={`h-1.5 w-1.5 rounded-full ${statusDot}`}
          />
          <span className="text-xs font-medium">{runtimeLabel}</span>
        </div>
        <p className="mt-0.5 font-mono text-[10px] text-muted-foreground/70">
          {(session.id ?? "").slice(0, 8)}…
        </p>
        <p className="mt-0.5 text-[10px] text-muted-foreground/60">{created}</p>
      </button>
    </li>
  );
}
