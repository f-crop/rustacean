import createClient, { type Client } from "openapi-fetch";
import type { paths } from "./generated/schema";

export type ApiClient = Client<paths>;

function resolveBaseUrl(): string {
  const fromEnv = import.meta.env.VITE_API_BASE_URL;
  if (fromEnv && fromEnv.length > 0) {
    return fromEnv.replace(/\/$/, "");
  }
  return "";
}

export const apiClient: ApiClient = createClient<paths>({
  baseUrl: resolveBaseUrl(),
  credentials: "include",
  headers: {
    "Content-Type": "application/json",
  },
});

// Track X-Trace-Id per Response without mutating the Response object.
const _responseTraceIds = new WeakMap<Response, string>();

apiClient.use({
  onResponse({ response }) {
    const id = response.headers.get("x-trace-id");
    if (id) {
      _responseTraceIds.set(response, id);
    }
    return undefined;
  },
});

export type ApiError = {
  status: number;
  body: unknown;
  traceId?: string;
};

export function toApiError(
  status: number,
  body: unknown,
  response?: Response,
): ApiError {
  const traceId = response ? _responseTraceIds.get(response) : undefined;
  const error: ApiError = { status, body };
  if (traceId) {
    error.traceId = traceId;
  }
  return error;
}

/** Returns the absolute URL for the API trace-redirect endpoint. */
export function traceRedirectUrl(traceId: string): string {
  return `${resolveBaseUrl()}/v1/traces/${traceId}`;
}
