import type { AuditEventItem } from "@/api";
import { formatApiError } from "@/lib/errors/api";
import { formatTimestamp } from "./utils";

interface MemberActivitySectionProps {
  readonly events: ReadonlyArray<AuditEventItem>;
  readonly isLoading: boolean;
  readonly isError: boolean;
  readonly error: { status: number; body: unknown } | null;
}

export function MemberActivitySection({
  events,
  isLoading,
  isError,
  error,
}: MemberActivitySectionProps): JSX.Element {
  if (isLoading) {
    return (
      <div className="rounded-lg border border-border bg-card p-4">
        <p className="text-sm text-muted-foreground">Loading…</p>
      </div>
    );
  }

  if (isError) {
    return (
      <div className="rounded-lg border border-border bg-card p-4">
        <p className="text-sm text-muted-foreground">
          {formatApiError(error, "Could not load member activity.")}
        </p>
      </div>
    );
  }

  if (events.length === 0) {
    return (
      <div className="rounded-lg border border-border bg-card p-4">
        <p className="text-sm text-muted-foreground">No user activity yet.</p>
      </div>
    );
  }

  return (
    <div className="overflow-x-auto rounded-lg border border-border">
      <table className="w-full text-sm" aria-label="Member activity events">
        <thead className="border-b border-border bg-muted/40">
          <tr>
            <th scope="col" className="px-4 py-2 text-left font-medium text-muted-foreground">
              Action
            </th>
            <th scope="col" className="px-4 py-2 text-left font-medium text-muted-foreground">
              Outcome
            </th>
            <th scope="col" className="px-4 py-2 text-left font-medium text-muted-foreground">
              Occurred at
            </th>
          </tr>
        </thead>
        <tbody>
          {events.map((event) => (
            <tr
              key={event.id}
              className="border-b border-border last:border-0 hover:bg-muted/20"
            >
              <td className="px-4 py-2 font-mono text-xs">{event.action}</td>
              <td className="px-4 py-2">
                <span
                  className={`text-xs font-medium ${
                    event.outcome === "success"
                      ? "text-green-600 dark:text-green-400"
                      : "text-destructive"
                  }`}
                >
                  {event.outcome}
                </span>
              </td>
              <td className="px-4 py-2 text-xs text-muted-foreground">
                {formatTimestamp(event.occurred_at)}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
