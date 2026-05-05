import { useEffect, useMemo } from "react";
import {
  BarChart,
  Bar,
  XAxis,
  YAxis,
  Tooltip,
  ResponsiveContainer,
  CartesianGrid,
} from "recharts";
import {
  useMe,
  useRepos,
  useAuditEvents,
  useRecentIngestions,
  useInvalidateRecentIngestions,
  type AuditEventItem,
  type RecentIngestionRun,
} from "@/api";
import { PageContainer } from "@/components/repos/PageContainer";
import { useEventStream } from "@/hooks/useEventStream";
import { formatApiError } from "@/lib/errors/api";

// ---------------------------------------------------------------------------
// ActivityPage — entry point
// ---------------------------------------------------------------------------

export function ActivityPage(): JSX.Element {
  const me = useMe({ retry: false });

  if (me.isLoading) {
    return (
      <PageContainer>
        <p className="text-sm text-muted-foreground">Loading session…</p>
      </PageContainer>
    );
  }

  if (me.isError || !me.data) {
    return (
      <PageContainer>
        <h1 className="text-2xl font-semibold tracking-tight">Activity</h1>
        <p className="mt-2 text-sm text-muted-foreground">
          Sign in to view tenant activity.
        </p>
      </PageContainer>
    );
  }

  return <ActivityPageInner tenantId={me.data.current_tenant.id} />;
}

// ---------------------------------------------------------------------------
// Inner page (tenant resolved)
// ---------------------------------------------------------------------------

interface ActivityPageInnerProps {
  readonly tenantId: string;
}

function ActivityPageInner({ tenantId }: ActivityPageInnerProps): JSX.Element {
  const apiBase = import.meta.env.VITE_API_BASE_URL ?? "";

  const repos = useRepos(tenantId);
  const allAudit = useAuditEvents(tenantId, { limit: 200 });
  const userAudit = useAuditEvents(tenantId, { limit: 100 });
  const queryAudit = useAuditEvents(tenantId, { action: "search.executed", limit: 50 });
  const recentIngestions = useRecentIngestions(tenantId);
  const invalidateIngestions = useInvalidateRecentIngestions();

  const { events, readyState } = useEventStream(`${apiBase}/v1/ingest/events`);

  // Refetch recent ingestions when a succeeded ingest SSE event arrives (AC5)
  useEffect(() => {
    const latestIngest = events
      .filter((e) => e.type === "ingest.status")
      .at(-1);
    if (!latestIngest) return;
    try {
      const parsed = JSON.parse(latestIngest.data) as { status?: string };
      if (parsed.status === "succeeded" || parsed.status === "done") {
        void invalidateIngestions(tenantId);
      }
    } catch {
      // malformed event — ignore
    }
  }, [events, tenantId, invalidateIngestions]);

  // Build audit-events-per-day data for the chart
  const auditChartData = useMemo(() => {
    if (!allAudit.data) return [];
    return buildDailyEventCounts(allAudit.data.events, 14);
  }, [allAudit.data]);

  // Filter member (user) activity client-side
  const memberEvents = useMemo(() => {
    if (!userAudit.data) return [];
    return userAudit.data.events.filter((e) => e.actor_kind === "user");
  }, [userAudit.data]);

  return (
    <div className="container max-w-5xl py-8 space-y-8">
      {/* Page header + SSE status */}
      <header className="flex flex-col gap-1">
        <div className="flex items-center justify-between">
          <h1 className="text-2xl font-semibold tracking-tight">Activity</h1>
          <SseStatusBadge readyState={readyState} />
        </div>
        <p className="text-sm text-muted-foreground">
          Ingestion runs, member activity, and recent queries for this workspace.
        </p>
      </header>

      {/* Summary cards */}
      <SummaryCards
        repoCount={repos.data?.repos.length ?? 0}
        auditTotal={allAudit.data?.total ?? 0}
        memberEventCount={memberEvents.length}
        isLoading={repos.isLoading || allAudit.isLoading}
      />

      {/* Activity over time chart */}
      <section aria-labelledby="chart-heading">
        <h2
          id="chart-heading"
          className="mb-3 text-base font-semibold tracking-tight"
        >
          Audit events (last 14 days)
        </h2>
        <div className="rounded-lg border border-border bg-card p-4">
          {allAudit.isLoading ? (
            <ChartSkeleton />
          ) : allAudit.isError ? (
            <p className="text-sm text-muted-foreground">
              {formatApiError(allAudit.error, "Could not load audit data.")}
            </p>
          ) : auditChartData.length === 0 ? (
            <p className="text-sm text-muted-foreground">No audit events yet.</p>
          ) : (
            <ResponsiveContainer width="100%" height={200}>
              <BarChart data={auditChartData} margin={{ top: 4, right: 8, bottom: 4, left: 0 }}>
                <CartesianGrid strokeDasharray="3 3" className="stroke-border" />
                <XAxis
                  dataKey="day"
                  tick={{ fontSize: 11 }}
                  className="fill-muted-foreground"
                />
                <YAxis
                  tick={{ fontSize: 11 }}
                  className="fill-muted-foreground"
                  allowDecimals={false}
                />
                <Tooltip
                  contentStyle={{ fontSize: 12 }}
                  labelStyle={{ fontWeight: 600 }}
                />
                <Bar dataKey="count" name="Events" className="fill-primary" radius={[3, 3, 0, 0]} />
              </BarChart>
            </ResponsiveContainer>
          )}
        </div>
      </section>

      {/* Recent ingestions table */}
      <section aria-labelledby="ingestions-heading">
        <h2
          id="ingestions-heading"
          className="mb-3 text-base font-semibold tracking-tight"
        >
          Recent ingestion runs
        </h2>
        <RecentIngestionsTable
          runs={recentIngestions.data?.runs ?? []}
          isLoading={recentIngestions.isLoading}
          isError={recentIngestions.isError}
          error={recentIngestions.error}
        />
      </section>

      {/* Member activity */}
      <section aria-labelledby="members-heading">
        <h2
          id="members-heading"
          className="mb-3 text-base font-semibold tracking-tight"
        >
          Member activity
        </h2>
        <MemberActivitySection
          events={memberEvents}
          isLoading={userAudit.isLoading}
          isError={userAudit.isError}
          error={userAudit.error}
        />
      </section>

      {/* Recent queries */}
      <section aria-labelledby="queries-heading">
        <h2
          id="queries-heading"
          className="mb-3 text-base font-semibold tracking-tight"
        >
          Recent queries
        </h2>
        <RecentQueriesSection
          events={queryAudit.data?.events ?? []}
          isLoading={queryAudit.isLoading}
          isError={queryAudit.isError}
          error={queryAudit.error}
        />
      </section>
    </div>
  );
}

// ---------------------------------------------------------------------------
// SSE status badge
// ---------------------------------------------------------------------------

function SseStatusBadge({
  readyState,
}: {
  readyState: "connecting" | "open" | "closed";
}): JSX.Element {
  return (
    <div className="flex items-center gap-1.5" aria-live="polite">
      <span
        aria-hidden="true"
        className={`h-2 w-2 rounded-full ${
          readyState === "open"
            ? "bg-green-500"
            : readyState === "connecting"
              ? "bg-yellow-500 animate-pulse"
              : "bg-muted-foreground"
        }`}
      />
      <span className="text-xs text-muted-foreground">
        {readyState === "open"
          ? "Live"
          : readyState === "connecting"
            ? "Connecting…"
            : "Offline"}
      </span>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Summary cards
// ---------------------------------------------------------------------------

interface SummaryCardsProps {
  readonly repoCount: number;
  readonly auditTotal: number;
  readonly memberEventCount: number;
  readonly isLoading: boolean;
}

function SummaryCards({
  repoCount,
  auditTotal,
  memberEventCount,
  isLoading,
}: SummaryCardsProps): JSX.Element {
  const cards = [
    { label: "Connected repos", value: repoCount },
    { label: "Total audit events", value: auditTotal },
    { label: "User actions", value: memberEventCount },
  ] as const;

  return (
    <div className="grid grid-cols-1 gap-4 sm:grid-cols-3" aria-label="Summary metrics">
      {cards.map(({ label, value }) => (
        <div
          key={label}
          className="rounded-lg border border-border bg-card p-4"
        >
          <p className="text-sm text-muted-foreground">{label}</p>
          <p className="mt-1 text-2xl font-semibold tabular-nums">
            {isLoading ? "—" : value.toLocaleString()}
          </p>
        </div>
      ))}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Recent ingestions table
// ---------------------------------------------------------------------------

interface RecentIngestionsTableProps {
  readonly runs: readonly RecentIngestionRun[];
  readonly isLoading: boolean;
  readonly isError: boolean;
  readonly error: { status: number; body: unknown } | null;
}

const STATUS_CELL_CLASS: Record<string, string> = {
  succeeded: "text-green-600 dark:text-green-400",
  failed: "text-destructive",
  running: "text-blue-600 dark:text-blue-400",
  queued: "text-muted-foreground",
};

function RunStatusCell({ status }: { status: string }): JSX.Element {
  const cls = STATUS_CELL_CLASS[status] ?? "text-muted-foreground";
  return <span className={`text-xs font-medium capitalize ${cls}`}>{status}</span>;
}

function RecentIngestionsTable({
  runs,
  isLoading,
  isError,
  error,
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
          {runs.map((run) => (
            <tr key={run.id} className="border-b border-border last:border-0 hover:bg-muted/20">
              <td className="px-4 py-2 font-mono text-xs text-muted-foreground">
                {run.id.slice(0, 8)}…
              </td>
              <td className="px-4 py-2">
                <RunStatusCell status={run.status} />
              </td>
              <td className="px-4 py-2 text-xs text-muted-foreground">
                {run.started_at ? formatTimestamp(run.started_at) : "—"}
              </td>
              <td className="px-4 py-2 text-xs text-muted-foreground">
                {run.finished_at ? formatTimestamp(run.finished_at) : "—"}
              </td>
              <td className="px-4 py-2 font-mono text-xs text-muted-foreground">
                {run.trace_id ? `${run.trace_id.slice(0, 8)}…` : "—"}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Member activity section
// ---------------------------------------------------------------------------

interface MemberActivitySectionProps {
  readonly events: ReadonlyArray<AuditEventItem>;
  readonly isLoading: boolean;
  readonly isError: boolean;
  readonly error: { status: number; body: unknown } | null;
}

function MemberActivitySection({
  events,
  isLoading,
  isError,
  error,
}: MemberActivitySectionProps): JSX.Element {
  if (isLoading) {
    return (
      <div className="rounded-lg border border-border bg-card p-4">
        <p className="text-sm text-muted-foreground">Loading…</p>
      </div>
    );
  }

  if (isError) {
    return (
      <div className="rounded-lg border border-border bg-card p-4">
        <p className="text-sm text-muted-foreground">
          {formatApiError(error, "Could not load member activity.")}
        </p>
      </div>
    );
  }

  if (events.length === 0) {
    return (
      <div className="rounded-lg border border-border bg-card p-4">
        <p className="text-sm text-muted-foreground">No user activity yet.</p>
      </div>
    );
  }

  return (
    <div className="overflow-x-auto rounded-lg border border-border">
      <table className="w-full text-sm" aria-label="Member activity events">
        <thead className="border-b border-border bg-muted/40">
          <tr>
            <th scope="col" className="px-4 py-2 text-left font-medium text-muted-foreground">
              Action
            </th>
            <th scope="col" className="px-4 py-2 text-left font-medium text-muted-foreground">
              Outcome
            </th>
            <th scope="col" className="px-4 py-2 text-left font-medium text-muted-foreground">
              Occurred at
            </th>
          </tr>
        </thead>
        <tbody>
          {events.map((event) => (
            <tr
              key={event.id}
              className="border-b border-border last:border-0 hover:bg-muted/20"
            >
              <td className="px-4 py-2 font-mono text-xs">{event.action}</td>
              <td className="px-4 py-2">
                <span
                  className={`text-xs font-medium ${
                    event.outcome === "success"
                      ? "text-green-600 dark:text-green-400"
                      : "text-destructive"
                  }`}
                >
                  {event.outcome}
                </span>
              </td>
              <td className="px-4 py-2 text-xs text-muted-foreground">
                {formatTimestamp(event.occurred_at)}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Recent queries section
// ---------------------------------------------------------------------------

interface RecentQueriesSectionProps {
  readonly events: ReadonlyArray<AuditEventItem>;
  readonly isLoading: boolean;
  readonly isError: boolean;
  readonly error: { status: number; body: unknown } | null;
}

function RecentQueriesSection({
  events,
  isLoading,
  isError,
  error,
}: RecentQueriesSectionProps): JSX.Element {
  if (isLoading) {
    return (
      <div className="rounded-lg border border-border bg-card p-4">
        <p className="text-sm text-muted-foreground">Loading…</p>
      </div>
    );
  }

  if (isError) {
    return (
      <div className="rounded-lg border border-border bg-card p-4">
        <p className="text-sm text-muted-foreground">
          {formatApiError(error, "Could not load recent queries.")}
        </p>
      </div>
    );
  }

  if (events.length === 0) {
    return (
      <div className="rounded-lg border border-border bg-card p-4">
        <p className="text-sm text-muted-foreground">
          No search queries recorded yet.
        </p>
      </div>
    );
  }

  return (
    <div className="overflow-x-auto rounded-lg border border-border">
      <table className="w-full text-sm" aria-label="Recent search queries">
        <thead className="border-b border-border bg-muted/40">
          <tr>
            <th scope="col" className="px-4 py-2 text-left font-medium text-muted-foreground">
              Action
            </th>
            <th scope="col" className="px-4 py-2 text-left font-medium text-muted-foreground">
              Outcome
            </th>
            <th scope="col" className="px-4 py-2 text-left font-medium text-muted-foreground">
              Occurred at
            </th>
          </tr>
        </thead>
        <tbody>
          {events.map((event) => (
            <tr
              key={event.id}
              className="border-b border-border last:border-0 hover:bg-muted/20"
            >
              <td className="px-4 py-2 font-mono text-xs">{event.action}</td>
              <td className="px-4 py-2">
                <span
                  className={`text-xs font-medium ${
                    event.outcome === "success"
                      ? "text-green-600 dark:text-green-400"
                      : "text-destructive"
                  }`}
                >
                  {event.outcome}
                </span>
              </td>
              <td className="px-4 py-2 text-xs text-muted-foreground">
                {formatTimestamp(event.occurred_at)}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Chart skeleton
// ---------------------------------------------------------------------------

function ChartSkeleton(): JSX.Element {
  return (
    <div
      role="status"
      aria-label="Loading chart"
      className="h-[200px] animate-pulse rounded bg-muted"
    />
  );
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

function formatTimestamp(iso: string): string {
  try {
    return new Date(iso).toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    return iso;
  }
}

interface DailyCount {
  day: string;
  count: number;
}

function buildDailyEventCounts(
  events: readonly AuditEventItem[],
  days: number,
): DailyCount[] {
  const now = new Date();
  const buckets = new Map<string, number>();

  for (let i = days - 1; i >= 0; i--) {
    const d = new Date(now);
    d.setDate(d.getDate() - i);
    const key = d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
    buckets.set(key, 0);
  }

  for (const event of events) {
    const d = new Date(event.occurred_at);
    const key = d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
    const existing = buckets.get(key);
    if (existing !== undefined) {
      buckets.set(key, existing + 1);
    }
  }

  return Array.from(buckets.entries()).map(([day, count]) => ({ day, count }));
}
