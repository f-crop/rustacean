import { useEventStream, type UseEventStreamResult } from "./useEventStream";

// Chat sessions reuse the Wave-7 event relay (ADR-013 §3).
// Event types emitted on GET /v1/chat/sessions/{id}/events mirror
// the agent session relay: session.event envelope + session.error lifecycle.
const CHAT_STREAM_EVENT_TYPES = ["session.event", "session.error"] as const;

export function useChatStream(
  sessionId: string | null,
  enabled = true,
): UseEventStreamResult {
  const apiBase = import.meta.env.VITE_API_BASE_URL ?? "";
  const url = sessionId ? `${apiBase}/v1/chat/sessions/${sessionId}/events` : "";

  return useEventStream(url, CHAT_STREAM_EVENT_TYPES, enabled && sessionId !== null);
}
