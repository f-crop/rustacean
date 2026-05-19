import { useMe } from "@/api";
import { PageContainer } from "@/components/repos/PageContainer";
import { useEventStream } from "@/hooks/useEventStream";

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

type PipelineStage = (typeof PIPELINE_STAGES)[number];
type StageStatus = "pending" | "running" | "done" | "error";
type IngestStatus =
  | "pending"
  | "processing"
  | "done"
  | "failed"
  | "unspecified"
  | "unknown";

interface IngestStatusEvent {
  ingest_request_id: string;
  tenant_id: string;
  status: IngestStatus;
  error_message: string;
  occurred_at_ms: number;
  stage: string | null;
  stage_seq: number;
  ingest_run_id: string;
}

interface StageState {
  readonly stage: PipelineStage;
  readonly status: StageStatus;
}

const STAGE_LABELS: Record<PipelineStage, string> = {
  clone: "Clone",
  expand: "Expand",
  parse: "Parse",
  typecheck: "Typecheck",
  extract: "Extract",
  embed: "Embed",
  project_pg: "Project (PostgreSQL)",
  project_neo4j: "Project (Neo4j)",
  project_qdrant: "Project (Qdrant)",
};

const PIPELINE_STAGE_SET = new Set<string>(PIPELINE_STAGES);

function parseIngestEvent(raw: string): IngestStatusEvent | null {
  try {
    return JSON.parse(raw) as IngestStatusEvent;
  } catch {
    return null;
  }
}

function mapIngestStatus(status: IngestStatus): StageStatus | null {
  switch (status) {
    case "processing":
      return "running";
    case "done":
      return "done";
    case "failed":
      return "error";
    default:
      return null;
  }
}

function deriveStageStates(
  events: ReadonlyArray<{ data: string }>,
): StageState[] {
  const byStage = new Map<string, StageStatus>(
    PIPELINE_STAGES.map((s) => [s, "pending"]),
  );

  for (const raw of events) {
    const e = parseIngestEvent(raw.data);
    if (!e || !e.stage || !PIPELINE_STAGE_SET.has(e.stage)) continue;
    const stageStatus = mapIngestStatus(e.status);
    if (stageStatus !== null) {
      byStage.set(e.stage, stageStatus);
    }
  }

  return PIPELINE_STAGES.map((s) => ({ stage: s, status: byStage.get(s)! }));
}

const STATUS_LABEL: Record<StageStatus, string> = {
  pending: "Pending",
  running: "Running",
  done: "Done",
  error: "Error",
};

const STATUS_COLOR: Record<StageStatus, string> = {
  pending: "text-muted-foreground",
  running: "text-blue-600 dark:text-blue-400",
  done: "text-green-600 dark:text-green-400",
  error: "text-destructive",
};

const STATUS_INDICATOR: Record<StageStatus, string> = {
  pending:
    "h-3 w-3 rounded-full border-2 border-muted-foreground/50 bg-transparent",
  running: "h-3 w-3 rounded-full bg-blue-500 animate-pulse",
  done: "h-3 w-3 rounded-full bg-green-500",
  error: "h-3 w-3 rounded-full bg-destructive",
};

interface StageRowProps {
  readonly state: StageState;
  readonly index: number;
  readonly isLast: boolean;
}

function StageRow({ state, index, isLast }: StageRowProps): JSX.Element {
  const label = STAGE_LABELS[state.stage];
  return (
    <li className="flex items-start gap-4">
      <div className="flex flex-col items-center">
        <div
          role="img"
          aria-label={`${label} stage: ${STATUS_LABEL[state.status]}`}
          className={STATUS_INDICATOR[state.status]}
        />
        {!isLast && (
          <div
            aria-hidden="true"
            className="mt-1 h-8 w-0.5 bg-border"
          />
        )}
      </div>
      <div className="pb-8 last:pb-0">
        <p className="text-sm font-medium">{label}</p>
        <p className={`text-xs ${STATUS_COLOR[state.status]}`}>
          {STATUS_LABEL[state.status]}
        </p>
      </div>
      <span className="sr-only">
        Step {index + 1} of {PIPELINE_STAGES.length}: {label},{" "}
        {STATUS_LABEL[state.status]}
      </span>
    </li>
  );
}

function IngestionTheatreInner(): JSX.Element {
  const apiBase = import.meta.env.VITE_API_BASE_URL ?? "";
  const { events, readyState } = useEventStream(
    `${apiBase}/v1/ingest/events`,
    ["ingest.status"],
  );

  const ingestEvents = events.filter((e) => e.type === "ingest.status");
  const stageStates = deriveStageStates(ingestEvents);
  const hasEvents = ingestEvents.length > 0;

  return (
    <PageContainer>
      <header className="mb-6 flex flex-col gap-1">
        <h1 className="text-2xl font-semibold tracking-tight">
          Ingestion Theatre
        </h1>
        <p className="text-sm text-muted-foreground">
          Live pipeline progress for this workspace
        </p>
      </header>

      <div className="flex items-center gap-2 mb-6">
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
        <span className="text-xs text-muted-foreground capitalize">
          {readyState === "open"
            ? "Connected — live"
            : readyState === "connecting"
              ? "Connecting…"
              : "Disconnected"}
        </span>
      </div>

      {!hasEvents ? (
        <section
          aria-label="No ingestion in progress"
          data-testid="ingestion-empty-state"
          className="rounded-lg border border-border bg-card p-6"
        >
          <h2 className="mb-2 text-sm font-medium">No ingestion in progress</h2>
          <p className="text-sm text-muted-foreground">
            Start an ingestion run from a repository to see live pipeline
            progress here.
          </p>

          <div aria-label="Pipeline stages — all pending" className="mt-6">
            <ol aria-label="Pipeline stages">
              {stageStates.map((state, i) => (
                <StageRow
                  key={state.stage}
                  state={state}
                  index={i}
                  isLast={i === PIPELINE_STAGES.length - 1}
                />
              ))}
            </ol>
          </div>
        </section>
      ) : (
        <section
          aria-label="Ingestion pipeline"
          data-testid="ingestion-active-state"
          className="rounded-lg border border-border bg-card p-6"
        >
          <h2 className="mb-4 text-sm font-medium">Pipeline progress</h2>

          <ol aria-label="Pipeline stages">
            {stageStates.map((state, i) => (
              <StageRow
                key={state.stage}
                state={state}
                index={i}
                isLast={i === PIPELINE_STAGES.length - 1}
              />
            ))}
          </ol>
        </section>
      )}
    </PageContainer>
  );
}

export function IngestionTheatre(): JSX.Element {
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
        <p className="text-sm text-muted-foreground">
          Sign in to view ingestion progress.
        </p>
      </PageContainer>
    );
  }

  return <IngestionTheatreInner />;
}
