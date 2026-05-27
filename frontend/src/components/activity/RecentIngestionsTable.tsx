import { Link } from "@tanstack/react-router";
import type { RecentIngestionRun } from "@/api";
import { formatApiError } from "@/lib/errors/api";
import { formatTimestamp } from "./utils";

const STATUS_CELL_CLASS: Record<string, string> = {
  succeeded: "text-green-600 dark:text-green-400",
  failed: "text-destructive",
  running: "text-blue-600 dark:text-blue-400",
  queued: "text-muted-foreground",
};

const TERMINAL_STATUSES = new Set(["succeeded", "failed", "cancelled"]);

function RunStatusCell({ status }: { status: string }): JSX.Element {
  const cls = STATUS_CELL_CLASS[status] ?? "text-muted-foreground";
  return <span className={`text-xs font-medium capitalize ${cls}`}>{status}</span>;
}

interface RecentIngestionsTableProps {
  readonly runs: readonly RecentIngestionRun[];
  readonly isLoading: boolean;
  readonly isError: boolean;
  readonly error: { status: number; body: unknown } | null;
  /** Map from run ID → "stage N/9" label for currently-running rows. */
  readonly currentStages?: Record<string, string>;
}

export function RecentIngestionsTable({
  runs,
  isLoading,
  isError,
  error,
  currentStages = {},
}: RecentIngestionsTableProps): JSX.Element {
  if (isLoading) {
    return (
      <div className="rounded-lg border border-border bg-card p-4">
        <p className="text-sm text-muted-foreground">Loading…</p>
      </div>
    );
  }

  if (isError) {
    const is404 = error?.status === 404;
    return (
      <div className="rounded-lg border border-border bg-card p-4">
        <p className="text-sm text-muted-foreground">
          {is404
            ? "The recent ingestions endpoint is not yet available. Waiting on backend."
            : formatApiError(error, "Could not load recent ingestions.")}
        </p>
        {is404 && (
          <p className="mt-1 text-xs text-muted-foreground">
            Tracked in{" "}
            <a
              href="https://github.com/jarnura/rustacean/issues/213"
              target="_blank"
              rel="noopener noreferrer"
              className="underline hover:text-foreground"
            >
              GitHub #213
            </a>
            .
          </p>
        )}
      </div>
    );
  }

  if (runs.length === 0) {
    return (
      <div className="rounded-lg border border-border bg-card p-4">
        <p className="text-sm text-muted-foreground">No ingestion runs found.</p>
      </div>
    );
  }

  return (
    <div className="overflow-x-auto rounded-lg border border-border">
      <table className="w-full text-sm" aria-label="Recent ingestion runs">
        <thead className="border-b border-border bg-muted/40">
          <tr>
            <th scope="col" className="px-4 py-2 text-left font-medium text-muted-foreground">
              Run ID
            </th>
            <th scope="col" className="px-4 py-2 text-left font-medium text-muted-foreground">
              Status
            </th>
            <th scope="col" className="px-4 py-2 text-left font-medium text-muted-foreground">
              Current stage
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
          {runs.map((run) => {
            const isTerminal = TERMINAL_STATUSES.has(run.status);
            const stageLabel = !isTerminal ? (currentStages[run.id] ?? null) : null;
            return (
              <tr key={run.id} className="border-b border-border last:border-0 hover:bg-muted/20">
                <td className="px-4 py-2 font-mono text-xs text-muted-foreground">
                  {run.id.slice(0, 8)}…
                </td>
                <td className="px-4 py-2">
                  <RunStatusCell status={run.status} />
                </td>
                <td className="px-4 py-2 text-xs text-muted-foreground" data-testid="stage-cell">
                  {stageLabel ?? "—"}
                </td>
                <td className="px-4 py-2 text-xs text-muted-foreground">
                  {run.started_at ? formatTimestamp(run.started_at) : "—"}
                </td>
                <td className="px-4 py-2 text-xs text-muted-foreground">
                  {isTerminal && run.finished_at
                    ? formatTimestamp(run.finished_at)
                    : "—"}
                </td>
                <td className="px-4 py-2 font-mono text-xs text-muted-foreground">
                  {run.trace_id ? (
                    <Link
                      to="/trace/$traceId"
                      params={{ traceId: run.trace_id }}
                      search={{ runId: run.id }}
                      className="hover:underline"
                    >
                      {run.trace_id.slice(0, 8)}…
                    </Link>
                  ) : "—"}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
