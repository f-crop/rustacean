import { useMemo, useEffect, useState } from "react";
import { Link, useParams } from "@tanstack/react-router";
import { toast } from "sonner";
import { useMe, useSessionDetail, useSessionHistory, apiClient, type EventItem } from "@/api";
import { useEventStream } from "@/hooks/useEventStream";
import {
  EventVirtualList,
  EVENT_FILTER_TYPES,
  type DisplayEvent,
  type EventFilterType,
} from "@/components/agent-execution/EventVirtualList";
import { PageContainer } from "@/components/repos/PageContainer";
import { routes } from "@/lib/routes";
import { formatTimestamp } from "@/components/activity/utils";
import { formatApiError } from "@/lib/errors/api";

// ---------------------------------------------------------------------------
// Status helpers
// ---------------------------------------------------------------------------

const STATUS_LABEL_CLASS: Record<string, string> = {
  succeeded: "text-green-600 dark:text-green-400",
  completed: "text-green-600 dark:text-green-400",
  done: "text-green-600 dark:text-green-400",
  failed: "text-destructive",
  running: "text-blue-600 dark:text-blue-400",
  processing: "text-blue-600 dark:text-blue-400",
  pending: "text-muted-foreground",
  cancelled: "text-muted-foreground",
};

const RUNNING_STATUSES = new Set(["running", "processing", "pending"]);

// ---------------------------------------------------------------------------
// SSE event types to subscribe to
// ---------------------------------------------------------------------------

const AGENT_SESSION_EVENT_TYPES = ["session.event", "session.error"] as const;

// ---------------------------------------------------------------------------
// Page root — guards auth + param
// ---------------------------------------------------------------------------

export function SessionReplayPage(): JSX.Element {
  const me = useMe({ retry: false });
  const { sessionId } = useParams({ strict: false });

  if (me.isLoading) {
    return (
      <PageContainer>
        <p className="text-sm text-muted-foreground">Loading…</p>
      </PageContainer>
    );
  }

  if (me.isError || !me.data) {
    return (
      <PageContainer>
        <p className="text-sm text-muted-foreground">Sign in to view session replay.</p>
      </PageContainer>
    );
  }

  if (!sessionId) {
    return (
      <PageContainer>
        <p className="text-sm text-destructive">Invalid session URL.</p>
      </PageContainer>
    );
  }

  return (
    <SessionReplayInner
      tenantId={me.data.current_tenant.id}
      sessionId={sessionId}
    />
  );
}

// ---------------------------------------------------------------------------
// Main inner component
// ---------------------------------------------------------------------------

interface SessionReplayInnerProps {
  readonly tenantId: string;
  readonly sessionId: string;
}

function SessionReplayInner({ tenantId, sessionId }: SessionReplayInnerProps): JSX.Element {
  const apiBase = (import.meta.env.VITE_API_BASE_URL as string | undefined) ?? "";
  const [activeFilters, setActiveFilters] = useState<Set<EventFilterType>>(new Set());

  // Session metadata — refetch every 5 s while running
  const session = useSessionDetail(tenantId, sessionId, { refetchInterval: 5_000 });

  // Load all historical event pages
  const history = useSessionHistory(sessionId, true);

  // Auto-fetch remaining pages once previous page resolves
  const { hasNextPage, isFetchingNextPage, fetchNextPage } = history;
  useEffect(() => {
    if (hasNextPage && !isFetchingNextPage) {
      void fetchNextPage();
    }
  }, [hasNextPage, isFetchingNextPage, fetchNextPage]);

  // Flatten history pages into ordered EventItem array
  const historyEvents = useMemo<EventItem[]>(() => {
    if (!history.data) return [];
    return history.data.pages.flatMap((p) => p.events);
  }, [history.data]);

  // Last sequence seen in history — SSE connects from here
  const lastHistorySeq = useMemo<number | null>(() => {
    if (historyEvents.length === 0) return null;
    return historyEvents[historyEvents.length - 1]?.sequence ?? null;
  }, [historyEvents]);

  const isRunning = session.data ? RUNNING_STATUSES.has(session.data.status) : false;
  const historyFullyLoaded = !history.isFetching && !history.hasNextPage;

  // Connect SSE only after history fully loaded and session is running
  const sseEnabled = isRunning && historyFullyLoaded;
  const sseUrl = useMemo(() => {
    const base = `${apiBase}/v1/agents/sessions/${sessionId}/events`;
    return lastHistorySeq != null ? `${base}?from_sequence=${lastHistorySeq}` : base;
  }, [apiBase, sessionId, lastHistorySeq]);

  const { events: sseEvents, readyState } = useEventStream(
    sseUrl,
    AGENT_SESSION_EVENT_TYPES,
    sseEnabled,
  );

  // Convert SSE events to DisplayEvent
  const liveEvents = useMemo<DisplayEvent[]>(() => {
    const result: DisplayEvent[] = [];
    let lifecycleSeq = (lastHistorySeq ?? 0) + 0.5;

    for (const e of sseEvents) {
      if (e.type === "stream-reset") continue;

      if (e.type === "session.error") {
        let payload: unknown = null;
        try { payload = JSON.parse(e.data); } catch { /* ignore */ }
        result.push({
          key: `lifecycle-${lifecycleSeq}`,
          sequence: lifecycleSeq,
          eventType: "lifecycle",
          payload,
        });
        lifecycleSeq += 1;
        continue;
      }

      // session.event
      let envelope: Record<string, unknown> | null = null;
      try { envelope = JSON.parse(e.data) as Record<string, unknown>; } catch { /* ignore */ }
      if (!envelope || typeof envelope.sequence !== "number") continue;

      result.push({
        key: `live-${envelope.sequence as number}`,
        sequence: envelope.sequence as number,
        eventType: typeof envelope.event_type === "string" ? envelope.event_type : "unknown",
        payload: envelope.payload,
      });
    }
    return result;
  }, [sseEvents, lastHistorySeq]);

  // Convert history EventItems to DisplayEvent
  const historicalDisplayEvents = useMemo<DisplayEvent[]>(() => {
    return historyEvents.map((e) => ({
      key: e.id,
      sequence: e.sequence,
      eventType: e.event_type,
      payload: e.payload,
      createdAt: e.created_at,
    }));
  }, [historyEvents]);

  // Deduplicate: prefer history; live events only if sequence > last history seq
  const allEvents = useMemo<DisplayEvent[]>(() => {
    const maxHistSeq = lastHistorySeq ?? -1;
    const uniqueLive = liveEvents.filter((e) => e.sequence > maxHistSeq);
    return [...historicalDisplayEvents, ...uniqueLive].sort(
      (a, b) => a.sequence - b.sequence,
    );
  }, [historicalDisplayEvents, liveEvents, lastHistorySeq]);

  // Apply filters
  const filteredEvents = useMemo<DisplayEvent[]>(() => {
    if (activeFilters.size === 0) return allEvents;
    return allEvents.filter((e) => activeFilters.has(e.eventType as EventFilterType));
  }, [allEvents, activeFilters]);

  const toggleFilter = (type: EventFilterType) => {
    setActiveFilters((prev) => {
      const next = new Set(prev);
      if (next.has(type)) {
        next.delete(type);
      } else {
        next.add(type);
      }
      return next;
    });
  };

  // NDJSON download — use parseAs:"blob" so openapi-fetch hands us the blob
  // directly.  Without parseAs, apiClient.GET returns a raw Response whose body
  // is already consumed by openapi-fetch's internal parsing step, making a
  // subsequent response.blob() call throw a TypeError (silent no-op).
  const handleDownload = async () => {
    try {
      const { data: blob, response } = await apiClient.GET(
        "/v1/agents/sessions/{id}/log.ndjson",
        {
          parseAs: "blob",
          params: { path: { id: sessionId } },
        },
      );
      if (!response.ok) {
        toast.error("Could not download session log.");
        return;
      }
      const objectUrl = URL.createObjectURL(blob as Blob);
      const a = document.createElement("a");
      a.href = objectUrl;
      a.download = `session-${sessionId}.ndjson`;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(objectUrl);
    } catch {
      toast.error("Could not download session log.");
    }
  };

  return (
    <PageContainer>
      {/* Breadcrumb */}
      <nav aria-label="Breadcrumb" className="mb-4">
        <Link
          to={routes.agentExecution}
          className="text-sm text-muted-foreground hover:text-foreground"
        >
          ← Back to sessions
        </Link>
      </nav>

      {/* Page header */}
      <header className="mb-6 flex flex-wrap items-start justify-between gap-4">
        <div className="min-w-0">
          <h1 className="text-2xl font-semibold tracking-tight">Session Replay</h1>
          <p
            className="mt-1 font-mono text-xs text-muted-foreground"
            title={sessionId}
          >
            {sessionId}
          </p>
        </div>
        <div className="flex items-center gap-2 shrink-0">
          {session.data && (
            <StatusBadge status={session.data.status} />
          )}
          <button
            type="button"
            onClick={() => { void handleDownload(); }}
            disabled={session.data?.status === "pending"}
            title={
              session.data?.status === "pending"
                ? "No events yet — session is pending"
                : undefined
            }
            className={
              session.data?.status === "pending"
                ? "rounded-md border border-border bg-card px-3 py-1.5 text-sm font-medium opacity-50 cursor-not-allowed"
                : "rounded-md border border-border bg-card px-3 py-1.5 text-sm font-medium hover:bg-muted/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            }
            aria-label="Download NDJSON log"
          >
            Download NDJSON
          </button>
        </div>
      </header>

      {/* Session metadata */}
      {session.isLoading ? (
        <p className="mb-6 text-sm text-muted-foreground">Loading session…</p>
      ) : session.isError ? (
        <p className="mb-6 text-sm text-destructive">
          {formatApiError(session.error, "Could not load session.")}
        </p>
      ) : session.data ? (
        <SessionMeta
          runtimeKind={session.data.runtime_kind}
          tokensUsed={session.data.tokens_used}
          tokenBudget={session.data.token_budget}
          createdAt={session.data.created_at}
          startedAt={session.data.started_at ?? null}
          completedAt={session.data.completed_at ?? null}
          exitCode={session.data.exit_code ?? null}
          failureReason={session.data.failure_reason ?? null}
          promptPreview={session.data.input_prompt_preview ?? null}
        />
      ) : null}

      {/* Filter chips */}
      <FilterChips activeFilters={activeFilters} onToggle={toggleFilter} />

      {/* Event list */}
      <section
        className="mt-4 rounded-lg border border-border bg-card"
        aria-labelledby="events-heading"
      >
        <div className="flex items-center justify-between border-b border-border px-4 py-3">
          <h2 id="events-heading" className="text-sm font-medium">
            Events
            {allEvents.length > 0 && (
              <span className="ml-2 text-xs text-muted-foreground tabular-nums">
                {filteredEvents.length === allEvents.length
                  ? allEvents.length.toLocaleString()
                  : `${filteredEvents.length.toLocaleString()} / ${allEvents.length.toLocaleString()}`}
              </span>
            )}
          </h2>
          <div className="flex items-center gap-3">
            {history.isFetching && (
              <span className="text-xs text-muted-foreground">Loading history…</span>
            )}
            {sseEnabled && (
              <SseIndicator readyState={readyState} />
            )}
          </div>
        </div>

        <div className="p-2">
          {history.isError ? (
            session.data != null && RUNNING_STATUSES.has(session.data.status) ? (
              <p className="p-4 text-sm text-muted-foreground">No events yet.</p>
            ) : (
              <p className="p-4 text-sm text-destructive">
                {formatApiError(history.error, "Could not load event history.")}
              </p>
            )
          ) : (
            <EventVirtualList events={filteredEvents} />
          )}
        </div>
      </section>
    </PageContainer>
  );
}

// ---------------------------------------------------------------------------
// Status badge
// ---------------------------------------------------------------------------

interface StatusBadgeProps {
  readonly status: string;
}

function StatusBadge({ status }: StatusBadgeProps): JSX.Element {
  const cls = STATUS_LABEL_CLASS[status] ?? "text-muted-foreground";
  const isPulsing = RUNNING_STATUSES.has(status);

  return (
    <div className="flex items-center gap-1.5">
      {isPulsing && (
        <span
          aria-hidden="true"
          className="h-2 w-2 rounded-full bg-blue-500 animate-pulse"
        />
      )}
      <span className={`text-sm font-medium capitalize ${cls}`}>{status}</span>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Session metadata grid
// ---------------------------------------------------------------------------

interface SessionMetaProps {
  readonly runtimeKind: string;
  readonly tokensUsed: number;
  readonly tokenBudget: number;
  readonly createdAt: string;
  readonly startedAt: string | null;
  readonly completedAt: string | null;
  readonly exitCode: number | null;
  readonly failureReason: string | null;
  readonly promptPreview: string | null;
}

const RUNTIME_LABELS: Record<string, string> = {
  claude_code: "Claude Code",
  opencode: "OpenCode",
};

function SessionMeta({
  runtimeKind,
  tokensUsed,
  tokenBudget,
  createdAt,
  startedAt,
  completedAt,
  exitCode,
  failureReason,
  promptPreview,
}: SessionMetaProps): JSX.Element {
  return (
    <dl className="mb-6 grid grid-cols-2 gap-x-6 gap-y-3 rounded-lg border border-border bg-card px-4 py-3 text-sm sm:grid-cols-3">
      <div>
        <dt className="text-xs text-muted-foreground">Runtime</dt>
        <dd className="mt-0.5">{RUNTIME_LABELS[runtimeKind] ?? runtimeKind}</dd>
      </div>
      <div>
        <dt className="text-xs text-muted-foreground">Tokens</dt>
        <dd className="mt-0.5 tabular-nums">
          {tokensUsed.toLocaleString()} / {tokenBudget.toLocaleString()}
        </dd>
      </div>
      <div>
        <dt className="text-xs text-muted-foreground">Created</dt>
        <dd className="mt-0.5">{formatTimestamp(createdAt)}</dd>
      </div>
      {startedAt ? (
        <div>
          <dt className="text-xs text-muted-foreground">Started</dt>
          <dd className="mt-0.5">{formatTimestamp(startedAt)}</dd>
        </div>
      ) : null}
      {completedAt ? (
        <div>
          <dt className="text-xs text-muted-foreground">Completed</dt>
          <dd className="mt-0.5">{formatTimestamp(completedAt)}</dd>
        </div>
      ) : null}
      {exitCode != null ? (
        <div>
          <dt className="text-xs text-muted-foreground">Exit code</dt>
          <dd className="mt-0.5 font-mono">{exitCode}</dd>
        </div>
      ) : null}
      {failureReason ? (
        <div className="col-span-2 sm:col-span-3">
          <dt className="text-xs text-muted-foreground">Failure reason</dt>
          <dd className="mt-0.5 text-destructive">{failureReason}</dd>
        </div>
      ) : null}
      {promptPreview ? (
        <div className="col-span-2 sm:col-span-3">
          <dt className="text-xs text-muted-foreground">Prompt preview</dt>
          <dd className="mt-0.5 text-muted-foreground">{promptPreview}</dd>
        </div>
      ) : null}
    </dl>
  );
}

// ---------------------------------------------------------------------------
// Filter chips
// ---------------------------------------------------------------------------

interface FilterChipsProps {
  readonly activeFilters: Set<EventFilterType>;
  readonly onToggle: (type: EventFilterType) => void;
}

const FILTER_LABELS: Record<EventFilterType, string> = {
  text: "Text",
  tool_use: "Tool Use",
  tool_result: "Tool Result",
  thinking: "Thinking",
  error: "Error",
  lifecycle: "Lifecycle",
};

function FilterChips({ activeFilters, onToggle }: FilterChipsProps): JSX.Element {
  const clearAll = () => {
    for (const t of EVENT_FILTER_TYPES) {
      if (activeFilters.has(t)) onToggle(t);
    }
  };

  return (
    <div className="flex flex-wrap gap-2" role="group" aria-label="Filter by event type">
      <span className="self-center text-xs text-muted-foreground">Filter:</span>
      {EVENT_FILTER_TYPES.map((type) => {
        const isActive = activeFilters.has(type);
        return (
          <button
            key={type}
            type="button"
            onClick={() => onToggle(type)}
            aria-pressed={isActive}
            className={
              isActive
                ? "rounded-full border border-primary bg-primary px-3 py-1 text-xs font-medium text-primary-foreground"
                : "rounded-full border border-border bg-card px-3 py-1 text-xs font-medium text-muted-foreground hover:border-primary/50 hover:text-foreground"
            }
          >
            {FILTER_LABELS[type]}
          </button>
        );
      })}
      {activeFilters.size > 0 && (
        <button
          type="button"
          onClick={clearAll}
          className="self-center text-xs text-muted-foreground underline hover:text-foreground"
        >
          Clear
        </button>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// SSE connection indicator
// ---------------------------------------------------------------------------

interface SseIndicatorProps {
  readonly readyState: "connecting" | "open" | "closed";
}

function SseIndicator({ readyState }: SseIndicatorProps): JSX.Element {
  const label =
    readyState === "open"
      ? "Live"
      : readyState === "connecting"
        ? "Connecting…"
        : "Disconnected";
  const color =
    readyState === "open"
      ? "bg-green-500"
      : readyState === "connecting"
        ? "bg-yellow-500 animate-pulse"
        : "bg-muted-foreground";

  return (
    <div className="flex items-center gap-1.5" aria-live="polite">
      <span aria-hidden="true" className={`h-2 w-2 rounded-full ${color}`} />
      <span className="text-xs text-muted-foreground">{label}</span>
    </div>
  );
}
