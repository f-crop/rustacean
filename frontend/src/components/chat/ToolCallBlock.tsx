import { useState } from "react";

interface ToolCallBlockProps {
  readonly name: string;
  readonly input: unknown;
  readonly result: unknown | null;
  readonly isError: boolean;
  readonly sequence: number;
}

export function ToolCallBlock({
  name,
  input,
  result,
  isError,
  sequence,
}: ToolCallBlockProps): JSX.Element {
  const [open, setOpen] = useState(false);
  const hasResult = result !== null;

  const containerClass = hasResult && isError
    ? "rounded border border-destructive/30 bg-destructive/5 px-3 py-2"
    : hasResult
      ? "rounded border border-green-200 bg-green-50/60 px-3 py-2 dark:border-green-900/40 dark:bg-green-950/20"
      : "rounded border border-blue-200 bg-blue-50/60 px-3 py-2 dark:border-blue-900/40 dark:bg-blue-950/20";

  const statusBadgeClass = hasResult && isError
    ? "rounded bg-destructive/10 px-1.5 py-0.5 text-xs font-medium text-destructive"
    : hasResult
      ? "rounded bg-green-100 px-1.5 py-0.5 text-xs font-medium text-green-700 dark:bg-green-900/40 dark:text-green-300"
      : "rounded bg-blue-100 px-1.5 py-0.5 font-mono text-xs font-medium text-blue-700 dark:bg-blue-900/40 dark:text-blue-300";

  const statusLabel = hasResult && isError ? "Error" : hasResult ? "Done" : "Running…";

  return (
    <div className={containerClass} data-testid="tool-call-block">
      <button
        type="button"
        className="flex w-full items-center gap-2 text-left"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        aria-label={`${name} tool call — ${statusLabel}`}
      >
        <span className="font-mono text-[10px] text-muted-foreground/50 tabular-nums">
          #{sequence}
        </span>
        <span className={statusBadgeClass}>{name}</span>
        <span className="text-xs text-muted-foreground">{statusLabel}</span>
        <span className="ml-auto text-xs text-muted-foreground" aria-hidden="true">
          {open ? "▲" : "▼"}
        </span>
      </button>
      {open && (
        <div className="mt-2 space-y-2">
          <div>
            <p className="mb-1 text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
              Input
            </p>
            <pre className="overflow-x-auto whitespace-pre text-xs text-muted-foreground">
              {JSON.stringify(input, null, 2)}
            </pre>
          </div>
          {hasResult && (
            <div>
              <p className="mb-1 text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
                Result
              </p>
              <pre className="overflow-x-auto whitespace-pre text-xs text-muted-foreground">
                {typeof result === "string" ? result : JSON.stringify(result, null, 2)}
              </pre>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
