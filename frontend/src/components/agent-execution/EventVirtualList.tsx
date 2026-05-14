import { useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";

// ---------------------------------------------------------------------------
// Unified display event — normalised from both history rows and SSE envelopes
// ---------------------------------------------------------------------------

export interface DisplayEvent {
  readonly key: string;
  readonly sequence: number;
  readonly eventType: string;
  readonly payload: unknown;
  readonly createdAt?: string;
}

// ---------------------------------------------------------------------------
// Filter chip types
// ---------------------------------------------------------------------------

export const EVENT_FILTER_TYPES = [
  "text",
  "tool_use",
  "tool_result",
  "thinking",
  "error",
  "lifecycle",
] as const;
export type EventFilterType = (typeof EVENT_FILTER_TYPES)[number];

// ---------------------------------------------------------------------------
// Virtual list
// ---------------------------------------------------------------------------

interface EventVirtualListProps {
  readonly events: ReadonlyArray<DisplayEvent>;
}

export function EventVirtualList({ events }: EventVirtualListProps): JSX.Element {
  const parentRef = useRef<HTMLDivElement>(null);

  const rowVirtualizer = useVirtualizer({
    count: events.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => 80,
    overscan: 8,
  });

  if (events.length === 0) {
    return (
      <div className="flex items-center justify-center py-12 text-sm text-muted-foreground">
        No events to display.
      </div>
    );
  }

  const virtualItems = rowVirtualizer.getVirtualItems();

  return (
    <div
      ref={parentRef}
      className="overflow-y-auto"
      style={{ height: "60vh" }}
      role="log"
      aria-label="Session events"
      aria-live="polite"
    >
      <div
        style={{
          height: rowVirtualizer.getTotalSize(),
          width: "100%",
          position: "relative",
        }}
      >
        {virtualItems.map((virtualRow) => {
          const event = events[virtualRow.index] as DisplayEvent | undefined;
          if (!event) return null;
          return (
            <div
              key={virtualRow.key}
              data-index={virtualRow.index}
              ref={rowVirtualizer.measureElement}
              style={{
                position: "absolute",
                top: 0,
                left: 0,
                width: "100%",
                transform: `translateY(${virtualRow.start}px)`,
              }}
              className="p-1"
            >
              <EventRow event={event} />
            </div>
          );
        })}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Event row dispatcher
// ---------------------------------------------------------------------------

interface EventRowProps {
  readonly event: DisplayEvent;
}

function EventRow({ event }: EventRowProps): JSX.Element {
  if (event.eventType === "lifecycle") {
    return <LifecycleRow event={event} />;
  }

  const payload = event.payload as Record<string, unknown> | null;
  if (payload == null || typeof payload !== "object") {
    return <RawRow event={event} />;
  }

  switch (payload.type) {
    case "text":
      return (
        <TextRow
          sequence={event.sequence}
          text={typeof payload.text === "string" ? payload.text : JSON.stringify(payload.text)}
        />
      );
    case "thinking":
      return (
        <ThinkingRow
          sequence={event.sequence}
          thinking={
            typeof payload.thinking === "string"
              ? payload.thinking
              : JSON.stringify(payload.thinking)
          }
        />
      );
    case "tool_use":
      return (
        <ToolUseRow
          sequence={event.sequence}
          id={typeof payload.id === "string" ? payload.id : "?"}
          name={typeof payload.name === "string" ? payload.name : "unknown"}
          input={payload.input}
        />
      );
    case "tool_result":
      return (
        <ToolResultRow
          sequence={event.sequence}
          toolUseId={
            typeof payload.tool_use_id === "string" ? payload.tool_use_id : "?"
          }
          content={payload.content}
          isError={payload.is_error === true}
        />
      );
    case "error":
      return (
        <ErrorRow
          sequence={event.sequence}
          message={typeof payload.message === "string" ? payload.message : "Unknown error"}
          code={typeof payload.code === "string" ? payload.code : undefined}
        />
      );
    case "user_input":
      return (
        <UserInputRow
          sequence={event.sequence}
          text={typeof payload.text === "string" ? payload.text : JSON.stringify(payload.text)}
        />
      );
    default:
      return <RawRow event={event} />;
  }
}

// ---------------------------------------------------------------------------
// Per-type row renderers
// ---------------------------------------------------------------------------

function SeqBadge({ sequence }: { readonly sequence: number }): JSX.Element {
  return (
    <span className="mt-0.5 shrink-0 font-mono text-[10px] text-muted-foreground/50 tabular-nums">
      #{sequence}
    </span>
  );
}

function TextRow({
  text,
  sequence,
}: {
  readonly text: string;
  readonly sequence: number;
}): JSX.Element {
  return (
    <div className="flex gap-2 items-start px-1 py-1.5">
      <SeqBadge sequence={sequence} />
      <p className="whitespace-pre-wrap text-sm leading-relaxed break-words min-w-0">{text}</p>
    </div>
  );
}

function ThinkingRow({
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
        <SeqBadge sequence={sequence} />
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

function ToolUseRow({
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
        <SeqBadge sequence={sequence} />
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

function ToolResultRow({
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
        <SeqBadge sequence={sequence} />
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

function ErrorRow({
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
        <SeqBadge sequence={sequence} />
        <span className="rounded bg-destructive/10 px-1.5 py-0.5 text-xs font-medium text-destructive">
          Error
        </span>
        {code && <span className="font-mono text-xs text-muted-foreground">{code}</span>}
      </div>
      <p className="mt-1 text-sm text-destructive">{message}</p>
    </div>
  );
}

function UserInputRow({
  text,
  sequence,
}: {
  readonly text: string;
  readonly sequence: number;
}): JSX.Element {
  return (
    <div className="rounded border border-border bg-muted/30 px-3 py-2">
      <div className="flex items-center gap-2 mb-1">
        <SeqBadge sequence={sequence} />
        <span className="rounded bg-muted px-1.5 py-0.5 text-xs font-medium text-muted-foreground">
          User
        </span>
      </div>
      <p className="whitespace-pre-wrap text-sm">{text}</p>
    </div>
  );
}

function LifecycleRow({ event }: { readonly event: DisplayEvent }): JSX.Element {
  const data = event.payload as Record<string, unknown> | null;
  const status = typeof data?.status === "string" ? data.status : "lifecycle";
  const message = typeof data?.message === "string" ? data.message : "";
  const isError = status !== "terminated" && status !== "cancelled" && status !== "completed";

  return (
    <div
      className={
        isError
          ? "rounded border border-destructive/30 bg-destructive/5 px-4 py-3 text-center"
          : "rounded border border-border bg-muted/20 px-4 py-3 text-center"
      }
      role="status"
    >
      <p className="text-sm font-medium capitalize">{status}</p>
      {message && <p className="text-xs text-muted-foreground">{message}</p>}
    </div>
  );
}

function RawRow({ event }: { readonly event: DisplayEvent }): JSX.Element {
  return (
    <div className="rounded border border-border/50 bg-muted/20 px-3 py-2">
      <div className="flex items-center gap-2 mb-1">
        <SeqBadge sequence={event.sequence} />
        <span className="rounded bg-primary/10 px-1.5 py-0.5 font-mono text-xs text-primary">
          {event.eventType}
        </span>
      </div>
      <pre className="whitespace-pre-wrap text-xs text-muted-foreground">
        {typeof event.payload === "object" && event.payload !== null
          ? JSON.stringify(event.payload, null, 2)
          : String(event.payload ?? "")}
      </pre>
    </div>
  );
}
