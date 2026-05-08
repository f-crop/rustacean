import type { RecentIngestionRun } from "@/api";
import { formatApiError } from "@/lib/errors/api";
import { formatTimestamp } from "@/components/activity/utils";

const STATUS_CELL_CLASS: Record<string, string> = {
  succeeded: "text-green-600 dark:text-green-400",
  done: "text-green-600 dark:text-green-400",
  failed: "text-destructive",
  running: "text-blue-600 dark:text-blue-400",
  processing: "text-blue-600 dark:text-blue-400",
  queued: "text-muted-foreground",
  pending: "text-muted-foreground",
};

function RunStatusCell({ status }: { readonly status: string }): JSX.Element {
  const cls = STATUS_CELL_CLASS[status] ?? "text-muted-foreground";
  return <span className={`text-xs font-medium capitalize ${cls}`}>{status}</span>;
}

interface SessionHistoryProps {
  readonly sessions: readonly RecentIngestionRun[];
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
              Status
            </th>
            <th scope="col" className="px-4 py-2 text-left font-medium text-muted-foreground">
              Started
            </th>
            <th scope="col" className="px-4 py-2 text-left font-medium text-muted-foreground">
              Finished
            </th>
            <th scope="col" className="px-4 py-2 text-left font-medium text-muted-foreground">
              Trace
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
  readonly session: RecentIngestionRun;
}

function SessionRow({ session }: SessionRowProps): JSX.Element {
  return (
    <tr className="border-b border-border last:border-0 hover:bg-muted/20">
      <td className="px-4 py-2 font-mono text-xs text-muted-foreground">
        {session.id.slice(0, 8)}…
      </td>
      <td className="px-4 py-2">
        <RunStatusCell status={session.status} />
      </td>
      <td className="px-4 py-2 text-xs text-muted-foreground">
        {session.started_at ? formatTimestamp(session.started_at) : "—"}
      </td>
      <td className="px-4 py-2 text-xs text-muted-foreground">
        {session.finished_at ? formatTimestamp(session.finished_at) : "—"}
      </td>
      <td className="px-4 py-2">
        {session.trace_id ? (
          <span
            className="font-mono text-xs text-muted-foreground"
            title={session.trace_id}
          >
            {session.trace_id.slice(0, 8)}…
          </span>
        ) : (
          <span className="font-mono text-xs text-muted-foreground/50">—</span>
        )}
      </td>
    </tr>
  );
}
