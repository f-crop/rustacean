import { useEffect, useMemo, useRef } from "react";
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
  useIngestionStagesForRunningRuns,
} from "@/api";
import { PageContainer } from "@/components/repos/PageContainer";
import { useEventStream } from "@/hooks/useEventStream";
import { formatApiError } from "@/lib/errors/api";
import { RecentIngestionsTable } from "@/components/activity/RecentIngestionsTable";
import { MemberActivitySection } from "@/components/activity/MemberActivitySection";
import { RecentQueriesSection } from "@/components/activity/RecentQueriesSection";
import { buildDailyEventCounts } from "@/components/activity/utils";

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

interface ActivityPageInnerProps {
  readonly tenantId: string;
}

function ActivityPageInner({ tenantId }: ActivityPageInnerProps): JSX.Element {
  const apiBase = import.meta.env.VITE_API_BASE_URL ?? "";

  const repos = useRepos(tenantId);
  const allAudit = useAuditEvents(tenantId, { limit: 200 });
  const userAudit = useAuditEvents(tenantId, { limit: 100 });
  const queryAudit = useAuditEvents(tenantId, { action: "search.executed", limit: 50 });
  const recentIngestions = useRecentIngestions(tenantId, 50, {
    refetchInterval: (query) => {
      const hasActive = query.state.data?.runs.some(
        (r) => r.status === "running" || r.status === "queued",
      );
      return hasActive ? 10_000 : false;
    },
  });
  const invalidateIngestions = useInvalidateRecentIngestions();

  const runningRunIds = useMemo(
    () =>
      (recentIngestions.data?.runs ?? [])
        .filter((r) => r.status === "running")
        .map((r) => r.id),
    [recentIngestions.data],
  );
  const allRunIds = useMemo(
    () => (recentIngestions.data?.runs ?? []).map((r) => r.id),
    [recentIngestions.data],
  );
  const currentStages = useIngestionStagesForRunningRuns(runningRunIds);
  const effectiveStages = useStageLabelMemory(currentStages, allRunIds);

  const { events, readyState } = useEventStream(`${apiBase}/v1/ingest/events`, ["ingest.status"]);

  useEffect(() => {
    const latestIngest = events
      .filter((e) => e.type === "ingest.status")
      .at(-1);
    if (!latestIngest) return;
    try {
      const parsed = JSON.parse(latestIngest.data) as { status?: string };
      if (parsed.status === "succeeded" || parsed.status === "done" || parsed.status === "failed") {
        void invalidateIngestions(tenantId);
      }
    } catch {
      // malformed event — ignore
    }
  }, [events, tenantId, invalidateIngestions]);

  const auditChartData = useMemo(() => {
    if (!allAudit.data) return [];
    return buildDailyEventCounts(allAudit.data.events, 14);
  }, [allAudit.data]);

  const memberEvents = useMemo(() => {
    if (!userAudit.data) return [];
    return userAudit.data.events.filter((e) => e.actor_kind === "user");
  }, [userAudit.data]);

  return (
    <div className="container max-w-5xl py-8 space-y-8">
      <header className="flex flex-col gap-1">
        <div className="flex items-center justify-between">
          <h1 className="text-2xl font-semibold tracking-tight">Activity</h1>
          <SseStatusBadge readyState={readyState} />
        </div>
        <p className="text-sm text-muted-foreground">
          Ingestion runs, member activity, and recent queries for this workspace.
        </p>
      </header>

      <SummaryCards
        repoCount={repos.data?.repos.length ?? 0}
        auditTotal={allAudit.data?.total ?? 0}
        memberEventCount={memberEvents.length}
        isLoading={repos.isLoading || allAudit.isLoading}
      />

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
          currentStages={effectiveStages}
        />
      </section>

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

// Retains the last-seen stage label per run so the cell never blanks between poll cycles.
function useStageLabelMemory(
  currentStages: Record<string, string>,
  allRunIds: readonly string[],
): Record<string, string> {
  const cacheRef = useRef<Record<string, string>>({});

  useEffect(() => {
    Object.assign(cacheRef.current, currentStages);
    const knownSet = new Set(allRunIds);
    for (const id of Object.keys(cacheRef.current)) {
      if (!knownSet.has(id)) {
        delete cacheRef.current[id];
      }
    }
  }, [currentStages, allRunIds]);

  return useMemo(
    () => ({ ...cacheRef.current, ...currentStages }),
    [currentStages],
  );
}

function ChartSkeleton(): JSX.Element {
  return (
    <div
      role="status"
      aria-label="Loading chart"
      className="h-[200px] animate-pulse rounded bg-muted"
    />
  );
}
