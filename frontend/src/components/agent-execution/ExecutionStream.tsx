import { useEventStream, type StreamedEvent } from "@/hooks/useEventStream";

interface ExecutionStreamProps {
  readonly streamUrl: string;
}

export function ExecutionStream({ streamUrl }: ExecutionStreamProps): JSX.Element {
  const { events, lastEventId, readyState } = useEventStream(streamUrl);

  const agentEvents = events.filter(
    (e) => e.type !== "stream-reset",
  );

  return (
    <section
      aria-label="Live execution stream"
      className="rounded-lg border border-border bg-card"
    >
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

      <div className="max-h-80 overflow-y-auto p-4">
        {agentEvents.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            No events yet. Events will appear here when an execution is running.
          </p>
        ) : (
          <ul aria-label="Stream events" className="space-y-2">
            {agentEvents.map((event, i) => (
              <EventRow key={event.id ?? i} event={event} />
            ))}
          </ul>
        )}
      </div>
    </section>
  );
}

function ConnectionIndicator({
  readyState,
}: {
  readonly readyState: "connecting" | "open" | "closed";
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

function EventRow({ event }: { readonly event: StreamedEvent }): JSX.Element {
  let parsedData: unknown;
  try {
    parsedData = JSON.parse(event.data);
  } catch {
    parsedData = event.data;
  }

  const isObject = typeof parsedData === "object" && parsedData !== null;

  return (
    <li className="rounded border border-border/50 bg-muted/20 px-3 py-2">
      <div className="flex items-center gap-2 mb-1">
        <span className="rounded bg-primary/10 px-1.5 py-0.5 font-mono text-xs text-primary">
          {event.type}
        </span>
        {event.id && (
          <span className="font-mono text-xs text-muted-foreground">
            #{event.id.slice(0, 6)}
          </span>
        )}
      </div>
      {isObject ? (
        <pre className="whitespace-pre-wrap text-xs text-muted-foreground">
          {JSON.stringify(parsedData, null, 2)}
        </pre>
      ) : (
        <p className="text-xs text-muted-foreground">{String(parsedData)}</p>
      )}
    </li>
  );
}
