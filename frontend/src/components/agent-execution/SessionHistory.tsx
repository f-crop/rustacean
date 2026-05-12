import type { SessionItem } from "@/api";
import { formatApiError } from "@/lib/errors/api";
import { formatTimestamp } from "@/components/activity/utils";

const STATUS_CELL_CLASS: Record<string, string> = {
  succeeded: "text-green-600 dark:text-green-400",
  completed: "text-green-600 dark:text-green-400",
  done: "text-green-600 dark:text-green-400",
  failed: "text-destructive",
  running: "text-blue-600 dark:text-blue-400",
  processing: "text-blue-600 dark:text-blue-400",
  pending: "text-muted-foreground",
  cancelled: "text-muted-foreground",
};

function StatusCell({ status }: { readonly status: string }): JSX.Element {
  const cls = STATUS_CELL_CLASS[status] ?? "text-muted-foreground";
  return <span className={`text-xs font-medium capitalize ${cls}`}>{status}</span>;
}

function RuntimeBadge({ kind }: { readonly kind: string }): JSX.Element {
  const labels: Record<string, string> = {
    claude_code: "Claude Code",
    opencode: "OpenCode",
  };
  return (
    <span className="rounded bg-muted px-1.5 py-0.5 text-xs text-muted-foreground">
      {labels[kind] ?? kind}
    </span>
  );
}

interface SessionHistoryProps {
  readonly sessions: readonly SessionItem[];
  readonly isLoading: boolean;
  readonly isError: boolean;
  readonly error: { status: number; body: unknown } | null;
  readonly onDelete: (id: string) => void;
  readonly deletingId: string | null;
}

export function SessionHistory({
  sessions,
  isLoading,
  isError,
  error,
  onDelete,
  deletingId,
}: SessionHistoryProps): JSX.Element {
  if (isLoading) {
    return (
      <div className="rounded-lg border border-border bg-card p-4">
        <p className="text-sm text-muted-foreground">Loading sessions…</p>
      </div>
    );
  }

  if (isError) {
    return (
      <div className="rounded-lg border border-border bg-card p-4">
        <p className="text-sm text-destructive">
          {formatApiError(error, "Could not load session history.")}
        </p>
      </div>
    );
  }

  if (sessions.length === 0) {
    return (
      <div className="rounded-lg border border-border bg-card p-4">
        <p className="text-sm text-muted-foreground">No execution sessions found.</p>
      </div>
    );
  }

  return (
    <div className="overflow-x-auto rounded-lg border border-border">
      <table className="w-full text-sm" aria-label="Execution session history">
        <thead className="border-b border-border bg-muted/40">
          <tr>
            <th scope="col" className="px-4 py-2 text-left font-medium text-muted-foreground">
              Session
            </th>
            <th scope="col" className="px-4 py-2 text-left font-medium text-muted-foreground">
              Runtime
            </th>
            <th scope="col" className="px-4 py-2 text-left font-medium text-muted-foreground">
              Status
            </th>
            <th scope="col" className="px-4 py-2 text-left font-medium text-muted-foreground">
              Created
            </th>
            <th scope="col" className="px-4 py-2 text-left font-medium text-muted-foreground">
              Started
            </th>
            <th scope="col" className="px-4 py-2 text-right font-medium text-muted-foreground">
              Tokens
            </th>
            <th scope="col" className="px-4 py-2 text-right font-medium text-muted-foreground">
              Actions
            </th>
          </tr>
        </thead>
        <tbody>
          {sessions.map((session) => (
            <SessionRow
              key={session.id}
              session={session}
              onDelete={onDelete}
              deletingId={deletingId}
            />
          ))}
        </tbody>
      </table>
    </div>
  );
}

interface SessionRowProps {
  readonly session: SessionItem;
  readonly onDelete: (id: string) => void;
  readonly deletingId: string | null;
}

function SessionRow({ session, onDelete, deletingId }: SessionRowProps): JSX.Element {
  const canDelete = session.status === "pending" || session.status === "running";
  const isThisRowDeleting = deletingId === session.id;

  return (
    <tr className="border-b border-border last:border-0 hover:bg-muted/20">
      <td className="px-4 py-2">
        <span className="font-mono text-xs text-muted-foreground" title={session.id}>
          {session.id.slice(0, 8)}…
        </span>
        {session.input_prompt_preview && (
          <p className="mt-0.5 max-w-[200px] truncate text-xs text-muted-foreground/70" title={session.input_prompt_preview}>
            {session.input_prompt_preview}
          </p>
        )}
      </td>
      <td className="px-4 py-2">
        <RuntimeBadge kind={session.runtime_kind} />
      </td>
      <td className="px-4 py-2">
        <StatusCell status={session.status} />
      </td>
      <td className="px-4 py-2 text-xs text-muted-foreground">
        {formatTimestamp(session.created_at)}
      </td>
      <td className="px-4 py-2 text-xs text-muted-foreground">
        {session.started_at ? formatTimestamp(session.started_at) : "—"}
      </td>
      <td className="px-4 py-2 text-right text-xs tabular-nums text-muted-foreground">
        {session.tokens_used.toLocaleString()} / {session.token_budget.toLocaleString()}
      </td>
      <td className="px-4 py-2 text-right">
        {canDelete ? (
          <button
            type="button"
            onClick={() => onDelete(session.id)}
            disabled={isThisRowDeleting}
            className="rounded px-2 py-1 text-xs text-destructive hover:bg-destructive/10 disabled:cursor-not-allowed disabled:opacity-50"
            aria-label={`Terminate session ${session.id.slice(0, 8)}`}
          >
            {isThisRowDeleting ? "Stopping…" : "Stop"}
          </button>
        ) : (
          <span className="text-xs text-muted-foreground/50">—</span>
        )}
      </td>
    </tr>
  );
}
