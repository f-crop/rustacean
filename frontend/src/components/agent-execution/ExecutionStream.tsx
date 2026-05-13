import { useState } from "react";
import type { EventStreamReadyState, StreamedEvent } from "@/hooks/useEventStream";

// ---------------------------------------------------------------------------
// SSE payload types
// ---------------------------------------------------------------------------

type RuntimePayload =
  | { type: "text"; text: string }
  | { type: "thinking"; thinking: string }
  | { type: "tool_use"; id: string; name: string; input: unknown }
  | { type: "tool_result"; tool_use_id: string; content: unknown; is_error: boolean }
  | { type: "error"; message: string; code?: string }
  | { type: "user_input"; text: string };

interface SessionEventEnvelope {
  session_id: string;
  event_type: string;
  sequence: number;
  payload: RuntimePayload;
}

interface SessionErrorEnvelope {
  error: string;
  status: string;
  message: string;
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

interface ExecutionStreamProps {
  readonly events: ReadonlyArray<StreamedEvent>;
  readonly lastEventId: string | null;
  readonly readyState: EventStreamReadyState;
}

export function ExecutionStream({ events, lastEventId, readyState }: ExecutionStreamProps): JSX.Element {
  const rendered = events.filter((e) => e.type !== "stream-reset");

  return (
    <section aria-label="Live execution stream" className="rounded-lg border border-border bg-card">
      <div className="flex items-center justify-between border-b border-border px-4 py-3">
        <h2 className="text-sm font-medium">Live Stream</h2>
        <div className="flex items-center gap-2">
          <ConnectionIndicator readyState={readyState} />
          {lastEventId && (
            <span className="font-mono text-xs text-muted-foreground">
              last: {lastEventId.slice(0, 8)}…
            </span>
          )}
        </div>
      </div>

      <div className="max-h-[32rem] overflow-y-auto p-4">
        {rendered.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            No events yet. Events will appear here when an execution is running.
          </p>
        ) : (
          <div aria-label="Stream events" className="space-y-2">
            {rendered.map((event, i) => (
              <EventRenderer key={event.id ?? i} event={event} />
            ))}
          </div>
        )}
      </div>
    </section>
  );
}

// ---------------------------------------------------------------------------
// Event routing
// ---------------------------------------------------------------------------

function EventRenderer({ event }: { readonly event: StreamedEvent }): JSX.Element {
  if (event.type === "session.error") {
    const parsed = tryParseJson<SessionErrorEnvelope>(event.data);
    return <SessionLifecycleBanner data={parsed} />;
  }

  if (event.type === "session.event") {
    const envelope = tryParseJson<unknown>(event.data);
    if (isSessionEventEnvelope(envelope)) {
      return <PayloadRenderer envelope={envelope} />;
    }
  }

  return <RawEventRow event={event} />;
}

function isSessionEventEnvelope(v: unknown): v is SessionEventEnvelope {
  if (typeof v !== "object" || v === null) return false;
  const o = v as Record<string, unknown>;
  if (typeof o.event_type !== "string" || typeof o.sequence !== "number") return false;
  if (typeof o.payload !== "object" || o.payload === null) return false;
  return typeof (o.payload as Record<string, unknown>).type === "string";
}

function PayloadRenderer({ envelope }: { readonly envelope: SessionEventEnvelope }): JSX.Element {
  const { payload, sequence } = envelope;
  switch (payload.type) {
    case "text":
      return <TextChunk text={payload.text} sequence={sequence} />;
    case "thinking":
      return <ThinkingBlock thinking={payload.thinking} sequence={sequence} />;
    case "tool_use":
      return <ToolUseCard id={payload.id} name={payload.name} input={payload.input} sequence={sequence} />;
    case "tool_result":
      return (
        <ToolResultCard
          toolUseId={payload.tool_use_id}
          content={payload.content}
          isError={payload.is_error}
          sequence={sequence}
        />
      );
    case "error":
      return <ErrorBlock message={payload.message} code={payload.code} sequence={sequence} />;
    case "user_input":
      return <UserInputBlock text={payload.text} sequence={sequence} />;
  }
}

// ---------------------------------------------------------------------------
// Per-type renderers
// ---------------------------------------------------------------------------

function TextChunk({ text, sequence }: { readonly text: string; readonly sequence: number }): JSX.Element {
  return (
    <div className="flex gap-2 items-start">
      <span className="mt-0.5 shrink-0 font-mono text-[10px] text-muted-foreground/50 tabular-nums">
        #{sequence}
      </span>
      <p className="whitespace-pre-wrap text-sm leading-relaxed">{text}</p>
    </div>
  );
}

function ThinkingBlock({
  thinking,
  sequence,
}: {
  readonly thinking: string;
  readonly sequence: number;
}): JSX.Element {
  const [open, setOpen] = useState(false);
  const preview = thinking.slice(0, 80);

  return (
    <div className="rounded border border-border/40 bg-muted/10 px-3 py-2">
      <button
        type="button"
        className="flex w-full items-center gap-2 text-left"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
      >
        <span className="font-mono text-[10px] text-muted-foreground/50 tabular-nums">#{sequence}</span>
        <span className="text-xs italic text-muted-foreground">
          {open ? "Thinking" : `Thinking: ${preview}${thinking.length > 80 ? "…" : ""}`}
        </span>
        <span className="ml-auto text-xs text-muted-foreground" aria-hidden="true">
          {open ? "▲" : "▼"}
        </span>
      </button>
      {open && (
        <p className="mt-2 whitespace-pre-wrap text-xs text-muted-foreground">{thinking}</p>
      )}
    </div>
  );
}

function ToolUseCard({
  id,
  name,
  input,
  sequence,
}: {
  readonly id: string;
  readonly name: string;
  readonly input: unknown;
  readonly sequence: number;
}): JSX.Element {
  const [open, setOpen] = useState(false);

  return (
    <div className="rounded border border-blue-200 bg-blue-50/60 px-3 py-2 dark:border-blue-900/40 dark:bg-blue-950/20">
      <button
        type="button"
        className="flex w-full items-center gap-2 text-left"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
      >
        <span className="font-mono text-[10px] text-muted-foreground/50 tabular-nums">#{sequence}</span>
        <span className="rounded bg-blue-100 px-1.5 py-0.5 font-mono text-xs font-medium text-blue-700 dark:bg-blue-900/40 dark:text-blue-300">
          {name}
        </span>
        <span className="font-mono text-[10px] text-muted-foreground">{id.slice(0, 12)}…</span>
        <span className="ml-auto text-xs text-muted-foreground" aria-hidden="true">
          {open ? "▲" : "▼"}
        </span>
      </button>
      {open && (
        <pre className="mt-2 overflow-x-auto whitespace-pre text-xs text-muted-foreground">
          {JSON.stringify(input, null, 2)}
        </pre>
      )}
    </div>
  );
}

function ToolResultCard({
  toolUseId,
  content,
  isError,
  sequence,
}: {
  readonly toolUseId: string;
  readonly content: unknown;
  readonly isError: boolean;
  readonly sequence: number;
}): JSX.Element {
  const [open, setOpen] = useState(false);
  const containerClass = isError
    ? "rounded border border-destructive/30 bg-destructive/5 px-3 py-2"
    : "rounded border border-green-200 bg-green-50/60 px-3 py-2 dark:border-green-900/40 dark:bg-green-950/20";
  const badgeClass = isError
    ? "rounded bg-destructive/10 px-1.5 py-0.5 text-xs font-medium text-destructive"
    : "rounded bg-green-100 px-1.5 py-0.5 text-xs font-medium text-green-700 dark:bg-green-900/40 dark:text-green-300";

  return (
    <div className={containerClass}>
      <button
        type="button"
        className="flex w-full items-center gap-2 text-left"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
      >
        <span className="font-mono text-[10px] text-muted-foreground/50 tabular-nums">#{sequence}</span>
        <span className={badgeClass}>{isError ? "Error" : "Result"}</span>
        <span className="font-mono text-[10px] text-muted-foreground">{toolUseId.slice(0, 12)}…</span>
        <span className="ml-auto text-xs text-muted-foreground" aria-hidden="true">
          {open ? "▲" : "▼"}
        </span>
      </button>
      {open && (
        <pre className="mt-2 overflow-x-auto whitespace-pre text-xs text-muted-foreground">
          {typeof content === "string" ? content : JSON.stringify(content, null, 2)}
        </pre>
      )}
    </div>
  );
}

function ErrorBlock({
  message,
  code,
  sequence,
}: {
  readonly message: string;
  readonly code: string | undefined;
  readonly sequence: number;
}): JSX.Element {
  return (
    <div className="rounded border border-destructive/30 bg-destructive/5 px-3 py-2">
      <div className="flex items-center gap-2">
        <span className="font-mono text-[10px] text-muted-foreground/50 tabular-nums">#{sequence}</span>
        <span className="rounded bg-destructive/10 px-1.5 py-0.5 text-xs font-medium text-destructive">
          Error
        </span>
        {code && <span className="font-mono text-xs text-muted-foreground">{code}</span>}
      </div>
      <p className="mt-1 text-sm text-destructive">{message}</p>
    </div>
  );
}

function UserInputBlock({
  text,
  sequence,
}: {
  readonly text: string;
  readonly sequence: number;
}): JSX.Element {
  return (
    <div className="rounded border border-border bg-muted/30 px-3 py-2">
      <div className="flex items-center gap-2 mb-1">
        <span className="font-mono text-[10px] text-muted-foreground/50 tabular-nums">#{sequence}</span>
        <span className="rounded bg-muted px-1.5 py-0.5 text-xs font-medium text-muted-foreground">
          User
        </span>
      </div>
      <p className="whitespace-pre-wrap text-sm">{text}</p>
    </div>
  );
}

function SessionLifecycleBanner({ data }: { readonly data: SessionErrorEnvelope | null }): JSX.Element {
  const status = data?.status ?? "unknown";
  const message = data?.message ?? "Session ended.";
  const isQuiet = status === "terminated" || status === "cancelled";

  return (
    <div
      className={
        isQuiet
          ? "rounded border border-border bg-muted/20 px-4 py-3 text-center"
          : "rounded border border-destructive/30 bg-destructive/5 px-4 py-3 text-center"
      }
      role="status"
      aria-live="polite"
    >
      <p className="text-sm font-medium capitalize">{status}</p>
      <p className="text-xs text-muted-foreground">{message}</p>
    </div>
  );
}

function RawEventRow({
  event,
}: {
  readonly event: Pick<StreamedEvent, "id" | "type" | "data">;
}): JSX.Element {
  let parsed: unknown;
  try {
    parsed = JSON.parse(event.data);
  } catch {
    parsed = event.data;
  }

  return (
    <div className="rounded border border-border/50 bg-muted/20 px-3 py-2">
      <div className="flex items-center gap-2 mb-1">
        <span className="rounded bg-primary/10 px-1.5 py-0.5 font-mono text-xs text-primary">
          {event.type}
        </span>
        {event.id && (
          <span className="font-mono text-xs text-muted-foreground">#{event.id.slice(0, 6)}</span>
        )}
      </div>
      <pre className="whitespace-pre-wrap text-xs text-muted-foreground">
        {typeof parsed === "object" && parsed !== null
          ? JSON.stringify(parsed, null, 2)
          : String(parsed)}
      </pre>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Connection indicator
// ---------------------------------------------------------------------------

function ConnectionIndicator({
  readyState,
}: {
  readonly readyState: EventStreamReadyState;
}): JSX.Element {
  const label =
    readyState === "open"
      ? "Connected"
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

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

function tryParseJson<T>(s: string): T | null {
  try {
    return JSON.parse(s) as T;
  } catch {
    return null;
  }
}
