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
  pending: "text-yellow-600 dark:text-yellow-400",
  cancelled: "text-muted-foreground",
  queued: "text-muted-foreground",
};

function SessionStatusCell({ status }: { readonly status: string }): JSX.Element {
  const cls = STATUS_CELL_CLASS[status] ?? "text-muted-foreground";
  return <span className={`text-xs font-medium capitalize ${cls}`}>{status}</span>;
}

function formatTokenUsage(used: number, budget: number): string {
  if (budget === 0) return `${used}`;
  return `${used.toLocaleString()} / ${budget.toLocaleString()}`;
}

interface SessionHistoryProps {
  readonly sessions: readonly SessionItem[];
  readonly isLoading: boolean;
  readonly isError: boolean;
  readonly error: { status: number; body: unknown } | null;
}

export function SessionHistory({
  sessions,
  isLoading,
  isError,
  error,
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
        <p className="text-sm text-muted-foreground">
          {error?.status === 404
            ? "Agent execution endpoint not yet available. Waiting on backend."
            : formatApiError(error, "Could not load session history.")}
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
              Started
            </th>
            <th scope="col" className="px-4 py-2 text-left font-medium text-muted-foreground">
              Completed
            </th>
            <th scope="col" className="px-4 py-2 text-left font-medium text-muted-foreground">
              Tokens
            </th>
          </tr>
        </thead>
        <tbody>
          {sessions.map((session) => (
            <SessionRow key={session.id} session={session} />
          ))}
        </tbody>
      </table>
    </div>
  );
}

interface SessionRowProps {
  readonly session: SessionItem;
}

function SessionRow({ session }: SessionRowProps): JSX.Element {
  return (
    <tr className="border-b border-border last:border-0 hover:bg-muted/20">
      <td className="px-4 py-2">
        <span className="font-mono text-xs text-muted-foreground" title={session.id}>
          {session.id.slice(0, 8)}…
        </span>
        {session.input_prompt_preview && (
          <p className="mt-0.5 max-w-[200px] truncate text-xs text-muted-foreground/70">
            {session.input_prompt_preview}
          </p>
        )}
      </td>
      <td className="px-4 py-2">
        <span className="inline-flex rounded bg-primary/10 px-1.5 py-0.5 font-mono text-xs text-primary">
          {session.runtime_kind}
        </span>
      </td>
      <td className="px-4 py-2">
        <SessionStatusCell status={session.status} />
      </td>
      <td className="px-4 py-2 text-xs text-muted-foreground">
        {session.started_at ? formatTimestamp(session.started_at) : "—"}
      </td>
      <td className="px-4 py-2 text-xs text-muted-foreground">
        {session.completed_at ? formatTimestamp(session.completed_at) : "—"}
      </td>
      <td className="px-4 py-2 text-xs text-muted-foreground">
        {formatTokenUsage(session.tokens_used, session.token_budget)}
      </td>
    </tr>
  );
}
