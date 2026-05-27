// Browser-direct client for the Grafana Tempo HTTP API.
// Tempo is an external observability backend — not our control-api — so
// it has no OpenAPI spec and cannot go through apiClient.
// Raw fetch is intentionally allowed here (see eslint.config.js override for tempo.ts).

export interface TempoTag {
  readonly key: string;
  readonly type: string;
  readonly value: unknown;
}

export interface TempoLogField {
  readonly key: string;
  readonly value: unknown;
}

export interface TempoLog {
  readonly timestamp: number;
  readonly fields: readonly TempoLogField[];
}

export interface TempoRef {
  readonly refType: string;
  readonly traceID: string;
  readonly spanID: string;
}

export interface TempoSpan {
  readonly traceID: string;
  readonly spanID: string;
  readonly operationName: string;
  readonly startTime: number;
  readonly duration: number;
  readonly tags?: readonly TempoTag[];
  readonly logs?: readonly TempoLog[];
  readonly references?: readonly TempoRef[];
  readonly processID: string;
}

export interface TempoProcess {
  readonly serviceName: string;
}

export interface TempoTrace {
  readonly traceID: string;
  readonly spans: readonly TempoSpan[];
  readonly processes: Record<string, TempoProcess>;
}

interface TempoResponse {
  readonly data: readonly TempoTrace[];
}

export type FetchTempoResult =
  | { readonly ok: true; readonly trace: TempoTrace }
  | { readonly ok: false; readonly reason: string };

export async function fetchTempoTrace(
  tempoUrl: string,
  traceId: string,
  signal?: AbortSignal,
): Promise<FetchTempoResult> {
  let response: Response;
  try {
    response = await fetch(`${tempoUrl}/api/traces/${traceId}`, {
      signal: signal ?? null,
      headers: { Accept: "application/json" },
    });
  } catch (err) {
    if (err instanceof Error && err.name === "AbortError") throw err;
    return { ok: false, reason: err instanceof Error ? err.message : "Network error" };
  }

  if (!response.ok) {
    return { ok: false, reason: `Tempo returned HTTP ${response.status}` };
  }

  let body: TempoResponse;
  try {
    body = (await response.json()) as TempoResponse;
  } catch {
    return { ok: false, reason: "Invalid JSON from Tempo" };
  }

  const trace = body.data?.[0];
  if (!trace) {
    return { ok: false, reason: "Trace not found in Tempo response" };
  }
  return { ok: true, trace };
}
