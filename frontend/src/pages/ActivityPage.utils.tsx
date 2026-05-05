import type { AuditEventItem } from "@/api";

// ---------------------------------------------------------------------------
// Chart skeleton
// ---------------------------------------------------------------------------

export function ChartSkeleton(): JSX.Element {
  return (
    <div
      role="status"
      aria-label="Loading chart"
      className="h-[200px] animate-pulse rounded bg-muted"
    />
  );
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

export function formatTimestamp(iso: string): string {
  try {
    return new Date(iso).toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    return iso;
  }
}

export interface DailyCount {
  day: string;
  count: number;
}

export function buildDailyEventCounts(
  events: readonly AuditEventItem[],
  days: number,
): DailyCount[] {
  const now = new Date();
  const buckets = new Map<string, number>();

  for (let i = days - 1; i >= 0; i--) {
    const d = new Date(now);
    d.setDate(d.getDate() - i);
    const key = d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
    buckets.set(key, 0);
  }

  for (const event of events) {
    const d = new Date(event.occurred_at);
    const key = d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
    const existing = buckets.get(key);
    if (existing !== undefined) {
      buckets.set(key, existing + 1);
    }
  }

  return Array.from(buckets.entries()).map(([day, count]) => ({ day, count }));
}
