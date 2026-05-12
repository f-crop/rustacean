import { useEffect, useRef, useState } from "react";

export type EventStreamReadyState = "connecting" | "open" | "closed";

export interface StreamedEvent {
  readonly id: string | null;
  readonly type: string;
  readonly data: string;
}

export interface UseEventStreamResult {
  readonly events: ReadonlyArray<StreamedEvent>;
  readonly lastEventId: string | null;
  readonly readyState: EventStreamReadyState;
}

const RECONNECT_DELAY_MS = 5_000;
const DEFAULT_EVENT_TYPES = ["message"] as const;

export function useEventStream(
  url: string,
  eventTypes?: readonly string[],
): UseEventStreamResult {
  const [events, setEvents] = useState<StreamedEvent[]>([]);
  const [lastEventId, setLastEventId] = useState<string | null>(null);
  const [readyState, setReadyState] = useState<EventStreamReadyState>("connecting");

  const cancelledRef = useRef(false);
  const eventTypesRef = useRef(eventTypes);
  eventTypesRef.current = eventTypes;

  useEffect(() => {
    cancelledRef.current = false;

    let es: EventSource | null = null;
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

    const connect = () => {
      if (cancelledRef.current) return;

      es = new EventSource(url, { withCredentials: true });
      setReadyState("connecting");

      es.onopen = () => {
        if (!cancelledRef.current) setReadyState("open");
      };

      const handleEvent = (event: MessageEvent) => {
        if (cancelledRef.current) return;
        const streamed: StreamedEvent = {
          id: event.lastEventId || null,
          type: event.type,
          data: event.data,
        };
        if (event.lastEventId) {
          setLastEventId(event.lastEventId);
        }
        setEvents((prev) => [...prev, streamed]);
      };

      const typesToListen = eventTypesRef.current ?? DEFAULT_EVENT_TYPES;
      for (const type of typesToListen) {
        es.addEventListener(type, handleEvent);
      }

      es.addEventListener("stream-reset", () => {
        if (!cancelledRef.current) setEvents([]);
      });

      es.onerror = () => {
        if (cancelledRef.current) return;
        setReadyState("closed");
        es?.close();
        reconnectTimer = setTimeout(connect, RECONNECT_DELAY_MS);
      };
    };

    connect();

    return () => {
      cancelledRef.current = true;
      if (reconnectTimer !== null) clearTimeout(reconnectTimer);
      es?.close();
      setReadyState("closed");
    };
  }, [url]);

  return { events, lastEventId, readyState };
}
