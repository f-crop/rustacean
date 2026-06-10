import { useMemo } from "react";
import { useEventStream, type UseEventStreamResult } from "./useEventStream";
import type { ChatSessionEventEnvelope } from "@/lib/chat-api";

// Chat sessions reuse the Wave-7 event relay (ADR-013 §3).
// Event types emitted on GET /v1/chat/sessions/{id}/events mirror
// the agent session relay: session.event envelope + session.error lifecycle.
const CHAT_STREAM_EVENT_TYPES = ["session.event", "session.error"] as const;

export interface UseChatStreamResult extends UseEventStreamResult {
  /** True while the SSE connection is open and the current turn has not yet
   *  received turn_complete or error. Derived from the SSE state machine;
   *  never based on mutation.isPending (AC-1 of RUSAA-1974). */
  isStreaming: boolean;
}

function parseEnvelopePayload(data: string): { type: string; stop_reason?: string } | null {
  try {
    const env = JSON.parse(data) as ChatSessionEventEnvelope;
    if (typeof env === "object" && env !== null && typeof env.payload === "object") {
      return env.payload as { type: string; stop_reason?: string };
    }
    return null;
  } catch {
    return null;
  }
}

export function useChatStream(
  sessionId: string | null,
  enabled = true,
): UseChatStreamResult {
  const apiBase = import.meta.env.VITE_API_BASE_URL ?? "";
  const url = sessionId ? `${apiBase}/v1/chat/sessions/${sessionId}/events` : "";

  const base = useEventStream(url, CHAT_STREAM_EVENT_TYPES, enabled && sessionId !== null);

  const isStreaming = useMemo(() => {
    if (base.readyState !== "open") return false;
    // Walk events: streaming = true after user_input until turn_complete(non-tool_use) or error.
    let pending = false;
    for (const event of base.events) {
      if (event.type === "session.error") {
        pending = false;
        continue;
      }
      if (event.type !== "session.event") continue;
      const payload = parseEnvelopePayload(event.data);
      if (!payload) continue;
      if (payload.type === "user_input") {
        pending = true;
      } else if (payload.type === "turn_complete" && payload.stop_reason !== "tool_use") {
        pending = false;
      } else if (payload.type === "error") {
        pending = false;
      }
    }
    return pending;
  }, [base.events, base.readyState]);

  return { ...base, isStreaming };
}
