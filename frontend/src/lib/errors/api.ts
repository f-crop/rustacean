// REQ-FE-02: map ApiError objects to user-friendly toast messages.
// Auth surfaces only need a small mapping table — anything outside the table
// falls back to the server-supplied message or a status-aware default.
import type { ApiError } from "@/api";

type ErrorBody = {
  readonly message?: unknown;
  readonly error?: unknown;
};

function asApiError(value: unknown): ApiError | null {
  if (!value || typeof value !== "object") {
    return null;
  }
  const candidate = value as { status?: unknown };
  if (typeof candidate.status !== "number") {
    return null;
  }
  return value as ApiError;
}

const STATUS_FALLBACKS: Record<number, string> = {
  400: "Please check the form and try again.",
  401: "Your credentials weren't accepted. Please try again.",
  403: "You don't have permission to do that.",
  404: "We couldn't find that account.",
  409: "That email is already registered.",
  410: "This link has expired. Please request a new one.",
  422: "Please check the form and try again.",
  423: "Account temporarily locked. Try again in a few minutes.",
  429: "Too many attempts. Please slow down and retry shortly.",
};

const NETWORK_FALLBACK = "We couldn't reach the server. Check your connection.";
const DEFAULT_FALLBACK = "Something went wrong. Please try again.";

function readMessage(body: unknown): string | null {
  if (!body || typeof body !== "object") {
    return null;
  }
  const candidate = body as ErrorBody;
  if (typeof candidate.message === "string" && candidate.message.length > 0) {
    return candidate.message;
  }
  if (typeof candidate.error === "string" && candidate.error.length > 0) {
    return candidate.error;
  }
  return null;
}

export function formatApiError(
  error: unknown,
  contextFallback?: string,
): string {
  const apiError = asApiError(error);
  if (!apiError) {
    return contextFallback ?? DEFAULT_FALLBACK;
  }
  if (apiError.status === 0) {
    return NETWORK_FALLBACK;
  }
  const fromBody = readMessage(apiError.body);
  let message =
    fromBody ??
    STATUS_FALLBACKS[apiError.status] ??
    contextFallback ??
    DEFAULT_FALLBACK;

  if (apiError.traceId) {
    message = `${message} [trace: ${apiError.traceId.slice(0, 8)}]`;
  }
  return message;
}
