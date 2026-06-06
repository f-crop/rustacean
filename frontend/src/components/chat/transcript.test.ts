import { describe, it, expect } from "vitest";
import { buildTranscript } from "./transcript";
import type { StreamedEvent } from "@/hooks/useEventStream";

function sseEvent(payload: Record<string, unknown>, sequence: number): StreamedEvent {
  return {
    id: null,
    type: "session.event",
    data: JSON.stringify({ session_id: "s1", event_type: payload.type, sequence, payload }),
  };
}

describe("buildTranscript — turn_complete flush", () => {
  it("flushes pending assistant (inProgress → false) when turn_complete arrives", () => {
    const events: StreamedEvent[] = [
      sseEvent({ type: "user_input", text: "hello" }, 1),
      sseEvent({ type: "text", text: "Hi there!" }, 2),
      sseEvent({ type: "turn_complete", stop_reason: "end_turn" }, 3),
    ];

    const items = buildTranscript(events);

    expect(items).toHaveLength(2);
    const [user, assistant] = items;
    expect(user?.kind).toBe("user");
    expect(assistant?.kind).toBe("assistant");
    // Must NOT be inProgress after turn_complete
    expect((assistant as { kind: string; inProgress?: boolean }).inProgress).toBeUndefined();
  });

  it("keeps inProgress true when turn_complete has not arrived", () => {
    const events: StreamedEvent[] = [
      sseEvent({ type: "user_input", text: "hello" }, 1),
      sseEvent({ type: "text", text: "Hi there!" }, 2),
    ];

    const items = buildTranscript(events);

    expect(items).toHaveLength(2);
    const assistant = items[1];
    expect(assistant?.kind).toBe("assistant");
    expect((assistant as { kind: string; inProgress?: boolean }).inProgress).toBe(true);
  });

  it("turn_complete on empty pending is a no-op (does not add empty assistant)", () => {
    const events: StreamedEvent[] = [
      sseEvent({ type: "turn_complete", stop_reason: "end_turn" }, 1),
    ];

    const items = buildTranscript(events);
    expect(items).toHaveLength(0);
  });

  it("multi-turn: turn_complete after each reply leaves both turns non-inProgress", () => {
    const events: StreamedEvent[] = [
      sseEvent({ type: "user_input", text: "turn one" }, 1),
      sseEvent({ type: "text", text: "Reply one" }, 2),
      sseEvent({ type: "turn_complete", stop_reason: "end_turn" }, 3),
      sseEvent({ type: "user_input", text: "turn two" }, 4),
      sseEvent({ type: "text", text: "Reply two" }, 5),
      sseEvent({ type: "turn_complete", stop_reason: "end_turn" }, 6),
    ];

    const items = buildTranscript(events);
    expect(items).toHaveLength(4);

    const assistants = items.filter((i) => i.kind === "assistant");
    expect(assistants).toHaveLength(2);
    for (const a of assistants) {
      expect((a as { kind: string; inProgress?: boolean }).inProgress).toBeUndefined();
    }
  });
});
