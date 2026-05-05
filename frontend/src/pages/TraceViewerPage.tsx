import { useParams, useSearch } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { fetchTempoTrace, type TempoTrace, type TempoSpan, type TempoProcess } from "@/api/tempo";
import { useStageTimeline } from "@/api/hooks/useTraceViewer";
import { PageContainer } from "@/components/repos/PageContainer";
import type { StageRunItem } from "@/api/hooks/useTraceViewer";

// ---------------------------------------------------------------------------
// Stage constants
// ---------------------------------------------------------------------------

const PIPELINE_STAGES = [
  "clone",
  "expand",
  "parse",
  "typecheck",
  "extract",
  "embed",
  "project_pg",
  "project_neo4j",
  "project_qdrant",
] as const;

// ---------------------------------------------------------------------------
// Duration formatting
// ---------------------------------------------------------------------------

function formatDuration(startedAt: string | null, finishedAt: string | null): string {
  if (!startedAt || !finishedAt) return "—";
  const ms = new Date(finishedAt).getTime() - new Date(startedAt).getTime();
  if (ms < 1000) return `${ms}ms`;
  const s = (ms / 1000).toFixed(1);
  return `${s}s`;
}

function formatTimestamp(iso: string | null): string {
  if (!iso) return "—";
  return new Date(iso).toLocaleTimeString();
}

// ---------------------------------------------------------------------------
// Tier 2 — Postgres stage timeline
// ---------------------------------------------------------------------------

const STAGE_STATUS_INDICATOR: Record<string, string> = {
  pending: "h-3 w-3 rounded-full border-2 border-muted-foreground/40 bg-transparent",
  running: "h-3 w-3 rounded-full bg-blue-500 animate-pulse",
  succeeded: "h-3 w-3 rounded-full bg-green-500",
  failed: "h-3 w-3 rounded-full bg-destructive",
};

const STAGE_STATUS_LABEL: Record<string, string> = {
  pending: "Pending",
  running: "Running",
  succeeded: "Succeeded",
  failed: "Failed",
};

const STAGE_STATUS_COLOR: Record<string, string> = {
  pending: "text-muted-foreground",
  running: "text-blue-600 dark:text-blue-400",
  succeeded: "text-green-600 dark:text-green-400",
  failed: "text-destructive",
};

interface StageRowProps {
  readonly stage: string;
  readonly stageData: StageRunItem | undefined;
  readonly isLast: boolean;
}

function StageRow({ stage, stageData, isLast }: StageRowProps): JSX.Element {
  const status = stageData?.status ?? "pending";
  const indicator = STAGE_STATUS_INDICATOR[status] ?? STAGE_STATUS_INDICATOR.pending;
  const label = STAGE_STATUS_LABEL[status] ?? status;
  const color = STAGE_STATUS_COLOR[status] ?? STAGE_STATUS_COLOR.pending;

  return (
    <li className="flex items-start gap-4">
      <div className="flex flex-col items-center">
        <div
          role="img"
          aria-label={`${stage} stage: ${label}`}
          className={indicator}
        />
        {!isLast && (
          <div aria-hidden="true" className="mt-1 h-10 w-0.5 bg-border" />
        )}
      </div>
      <div className="pb-10 last:pb-0 flex-1 min-w-0">
        <p className="text-sm font-medium capitalize">{stage.replace("_", " ")}</p>
        <p className={`text-xs ${color}`}>{label}</p>
        {stageData && (stageData.started_at || stageData.finished_at) && (
          <dl className="mt-1 grid grid-cols-3 gap-x-4 text-xs text-muted-foreground">
            <div>
              <dt className="sr-only">Start</dt>
              <dd>{formatTimestamp(stageData.started_at ?? null)}</dd>
            </div>
            <div>
              <dt className="sr-only">End</dt>
              <dd>{formatTimestamp(stageData.finished_at ?? null)}</dd>
            </div>
            <div>
              <dt className="sr-only">Duration</dt>
              <dd>{formatDuration(stageData.started_at ?? null, stageData.finished_at ?? null)}</dd>
            </div>
          </dl>
        )}
        {stageData?.error_message && (
          <p className="mt-1 text-xs text-destructive break-words">
            {stageData.error_message}
          </p>
        )}
      </div>
    </li>
  );
}

interface StageFallbackProps {
  readonly ingestionRunId: string;
  readonly traceId: string;
}

function StageFallback({ ingestionRunId, traceId }: StageFallbackProps): JSX.Element {
  const { data, isLoading, isError } = useStageTimeline(ingestionRunId);

  if (isLoading) {
    return (
      <div role="status" aria-live="polite" className="text-sm text-muted-foreground">
        Loading stage timeline…
      </div>
    );
  }

  if (isError || !data) {
    return (
      <p className="text-sm text-muted-foreground">
        Stage timeline unavailable for this trace ID.
      </p>
    );
  }

  const stageMap = new Map<string, StageRunItem>();
  for (const s of data.stages) {
    stageMap.set(s.stage, s);
  }

  return (
    <section aria-label="Stage timeline">
      <header className="mb-4 flex items-center justify-between">
        <h2 className="text-sm font-medium">Pipeline stage timeline</h2>
        <span className="text-xs text-muted-foreground font-mono">{traceId}</span>
      </header>
      <ol aria-label="Pipeline stages" className="list-none">
        {PIPELINE_STAGES.map((stage, i) => (
          <StageRow
            key={stage}
            stage={stage}
            stageData={stageMap.get(stage)}
            isLast={i === PIPELINE_STAGES.length - 1}
          />
        ))}
      </ol>
      <p className="mt-4 text-xs text-muted-foreground">
        Ingestion run:{" "}
        <span className="font-mono">{ingestionRunId}</span>
      </p>
    </section>
  );
}

// ---------------------------------------------------------------------------
// Tier 1 — Tempo span tree
// ---------------------------------------------------------------------------

interface SpanNodeProps {
  readonly span: TempoSpan;
  readonly minStart: number;
  readonly totalDuration: number;
  readonly processes: Record<string, TempoProcess>;
  readonly depth: number;
}

function SpanNode({ span, minStart, totalDuration, processes, depth }: SpanNodeProps): JSX.Element {
  const [expanded, setExpanded] = useState(true);
  const offsetMs = span.startTime / 1000 - minStart;
  const durationMs = span.duration / 1000;
  const leftPct = totalDuration > 0 ? (offsetMs / totalDuration) * 100 : 0;
  const widthPct = totalDuration > 0 ? Math.max((durationMs / totalDuration) * 100, 0.5) : 0.5;
  const serviceName = processes[span.processID]?.serviceName ?? span.processID;

  return (
    <li>
      <div
        className="flex items-center gap-2 rounded px-2 py-1 hover:bg-muted/50 cursor-pointer text-sm"
        style={{ paddingLeft: `${depth * 16 + 8}px` }}
        onClick={() => setExpanded((v) => !v)}
        role="button"
        tabIndex={0}
        onKeyDown={(e) => e.key === "Enter" && setExpanded((v) => !v)}
        aria-expanded={expanded}
      >
        <span className="w-4 text-muted-foreground text-xs">{expanded ? "▾" : "▸"}</span>
        <span className="flex-1 min-w-0 truncate font-mono text-xs">
          <span className="text-muted-foreground">{serviceName}/</span>
          {span.operationName}
        </span>
        <span className="text-xs text-muted-foreground shrink-0">
          {durationMs >= 1 ? `${durationMs.toFixed(0)}ms` : `<1ms`}
        </span>
      </div>
      {expanded && (
        <div className="relative h-2 mx-2 my-0.5 rounded bg-muted overflow-hidden">
          <div
            className="absolute h-full rounded bg-blue-500/70"
            style={{ left: `${leftPct}%`, width: `${widthPct}%` }}
          />
        </div>
      )}
    </li>
  );
}

interface TempoViewProps {
  readonly trace: TempoTrace;
}

function TempoView({ trace }: TempoViewProps): JSX.Element {
  if (trace.spans.length === 0) {
    return <p className="text-sm text-muted-foreground">Trace contains no spans.</p>;
  }

  const minStart = Math.min(...trace.spans.map((s) => s.startTime / 1000));
  const maxEnd = Math.max(...trace.spans.map((s) => s.startTime / 1000 + s.duration / 1000));
  const totalDuration = maxEnd - minStart;

  const spansByParent = new Map<string, TempoSpan[]>();
  const rootSpans: TempoSpan[] = [];

  for (const span of trace.spans) {
    const parentRef = span.references?.find((r) => r.refType === "CHILD_OF");
    if (parentRef) {
      const siblings = spansByParent.get(parentRef.spanID) ?? [];
      spansByParent.set(parentRef.spanID, [...siblings, span]);
    } else {
      rootSpans.push(span);
    }
  }

  function renderSpan(span: TempoSpan, depth: number): JSX.Element {
    const children = spansByParent.get(span.spanID) ?? [];
    return (
      <li key={span.spanID}>
        <SpanNode
          span={span}
          minStart={minStart}
          totalDuration={totalDuration}
          processes={trace.processes}
          depth={depth}
        />
        {children.length > 0 && (
          <ol aria-label="Child spans" className="list-none">
            {children.map((child) => renderSpan(child, depth + 1))}
          </ol>
        )}
      </li>
    );
  }

  return (
    <section aria-label="Tempo span tree">
      <header className="mb-4 flex items-center justify-between">
        <h2 className="text-sm font-medium">Distributed trace</h2>
        <span className="text-xs text-muted-foreground">
          {trace.spans.length} spans · {(totalDuration).toFixed(0)}ms total
        </span>
      </header>
      <div className="rounded-lg border border-border bg-card overflow-auto max-h-[60vh]">
        <ol aria-label="Trace spans" className="list-none py-1">
          {rootSpans.map((span) => renderSpan(span, 0))}
        </ol>
      </div>
    </section>
  );
}

// ---------------------------------------------------------------------------
// Tier 1 loader — fetch from Tempo via api/tempo.ts
// ---------------------------------------------------------------------------

type TempoState =
  | { kind: "idle" }
  | { kind: "loading" }
  | { kind: "success"; trace: TempoTrace }
  | { kind: "error"; message: string };

function useTempoTrace(traceId: string): TempoState {
  const [state, setState] = useState<TempoState>({ kind: "idle" });
  const tempoUrl = import.meta.env.VITE_TEMPO_URL as string | undefined;

  useEffect(() => {
    if (!tempoUrl) {
      setState({ kind: "error", message: "VITE_TEMPO_URL not configured" });
      return;
    }

    setState({ kind: "loading" });

    const controller = new AbortController();

    fetchTempoTrace(tempoUrl, traceId, controller.signal)
      .then((result) => {
        if (result.ok) {
          setState({ kind: "success", trace: result.trace });
        } else {
          setState({ kind: "error", message: result.reason });
        }
      })
      .catch((err: unknown) => {
        if (err instanceof Error && err.name === "AbortError") return;
        const message = err instanceof Error ? err.message : "Unknown Tempo error";
        setState({ kind: "error", message });
      });

    return () => controller.abort();
  }, [traceId, tempoUrl]);

  return state;
}

// ---------------------------------------------------------------------------
// Main page
// ---------------------------------------------------------------------------

interface TraceViewerInnerProps {
  readonly traceId: string;
  readonly ingestionRunId: string | undefined;
}

function TraceViewerInner({ traceId, ingestionRunId }: TraceViewerInnerProps): JSX.Element {
  const tempo = useTempoTrace(traceId);

  return (
    <PageContainer>
      <header className="mb-6 flex flex-col gap-1">
        <h1 className="text-2xl font-semibold tracking-tight">Trace viewer</h1>
        <p className="text-sm text-muted-foreground font-mono truncate">{traceId}</p>
      </header>

      {tempo.kind === "loading" && (
        <div
          role="status"
          aria-live="polite"
          className="mb-6 flex items-center gap-2 text-sm text-muted-foreground"
        >
          <span className="h-2 w-2 rounded-full bg-blue-500 animate-pulse" />
          Loading trace from Tempo…
        </div>
      )}

      {tempo.kind === "success" && (
        <div className="mb-8">
          <TempoView trace={tempo.trace} />
        </div>
      )}

      {(tempo.kind === "error" || tempo.kind === "idle") && ingestionRunId && (
        <div className="rounded-lg border border-border bg-card p-6">
          {tempo.kind === "error" && (
            <p className="mb-4 text-xs text-muted-foreground">
              Tempo unavailable ({tempo.message}). Showing pipeline stage timeline.
            </p>
          )}
          <StageFallback ingestionRunId={ingestionRunId} traceId={traceId} />
        </div>
      )}

      {(tempo.kind === "error" || tempo.kind === "idle") && !ingestionRunId && (
        <div className="rounded-lg border border-border bg-card p-6">
          <p className="text-sm text-muted-foreground">
            Tempo unavailable and no ingestion run linked to this trace ID.
          </p>
        </div>
      )}
    </PageContainer>
  );
}

export function TraceViewerPage(): JSX.Element {
  const { traceId } = useParams({ from: '/trace/$traceId' });
  const { runId: ingestionRunId } = useSearch({ from: '/trace/$traceId' });

  return <TraceViewerInner traceId={traceId} ingestionRunId={ingestionRunId} />;
}
