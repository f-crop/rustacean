import { useMe } from "@/api";
import { PageContainer } from "@/components/repos/PageContainer";
import { useEventStream } from "@/hooks/useEventStream";

const PIPELINE_STAGES = [
  "clone",
  "expand",
  "parse",
  "typecheck",
  "graph",
  "embed",
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
}

interface StageState {
  readonly stage: PipelineStage;
  readonly status: StageStatus;
}

function parseIngestEvent(raw: string): IngestStatusEvent | null {
  try {
    return JSON.parse(raw) as IngestStatusEvent;
  } catch {
    return null;
  }
}

function deriveStageStates(
  events: ReadonlyArray<{ data: string }>,
): StageState[] {
  const initial: StageState[] = PIPELINE_STAGES.map((stage) => ({
    stage,
    status: "pending",
  }));

  if (events.length === 0) return initial;

  const parsed = events
    .map((e) => parseIngestEvent(e.data))
    .filter((e): e is IngestStatusEvent => e !== null);

  if (parsed.length === 0) return initial;

  const latest = parsed[parsed.length - 1];
  if (!latest) return initial;

  switch (latest.status) {
    case "processing":
      return PIPELINE_STAGES.map((stage, i) => ({
        stage,
        status: i === 0 ? "running" : "pending",
      }));
    case "done":
      return PIPELINE_STAGES.map((stage) => ({ stage, status: "done" }));
    case "failed":
      return PIPELINE_STAGES.map((stage, i) => ({
        stage,
        status: i === 0 ? "error" : "pending",
      }));
    default:
      return initial;
  }
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
  return (
    <li className="flex items-start gap-4">
      <div className="flex flex-col items-center">
        <div
          role="img"
          aria-label={`${state.stage} stage: ${STATUS_LABEL[state.status]}`}
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
        <p className="text-sm font-medium capitalize">{state.stage}</p>
        <p className={`text-xs ${STATUS_COLOR[state.status]}`}>
          {STATUS_LABEL[state.status]}
        </p>
      </div>
      <span className="sr-only">
        Step {index + 1} of {PIPELINE_STAGES.length}: {state.stage},{" "}
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
