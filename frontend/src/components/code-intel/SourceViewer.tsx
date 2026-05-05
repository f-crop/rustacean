import type { ItemResponse } from "@/api/hooks/useCodeIntel";

interface SourceViewerProps {
  readonly item: ItemResponse;
}

export function SourceViewer({ item }: SourceViewerProps): JSX.Element {
  const hasSource = item.source_preview != null;
  const lines = hasSource ? item.source_preview!.split("\n") : [];
  const startLine = item.line_start ?? 1;

  return (
    <div className="flex h-full flex-col overflow-hidden">
      <header className="flex shrink-0 flex-col gap-0.5 border-b border-border bg-muted/40 px-4 py-2">
        <div className="flex items-center gap-2">
          <span className="font-mono text-xs font-semibold text-foreground">
            {item.fqn}
          </span>
          <span className="rounded bg-muted px-1.5 py-0.5 font-mono text-[10px] text-muted-foreground uppercase">
            {item.kind}
          </span>
        </div>
        {item.source_path && (
          <p className="font-mono text-[11px] text-muted-foreground">
            {item.source_path}
            {item.line_start != null && (
              <span>
                {" "}:{item.line_start}
                {item.line_end != null && item.line_end !== item.line_start && (
                  <span>–{item.line_end}</span>
                )}
              </span>
            )}
          </p>
        )}
      </header>

      <div className="flex-1 overflow-auto">
        {hasSource ? (
          <pre
            aria-label={`Source for ${item.fqn}`}
            className="min-h-full bg-[#0d1117] p-4 font-mono text-xs leading-relaxed text-[#e6edf3]"
          >
            {lines.map((line, i) => {
              const lineNum = startLine + i;
              const isHighlighted =
                item.line_start != null &&
                item.line_end != null &&
                lineNum >= item.line_start &&
                lineNum <= item.line_end;
              return (
                <div
                  key={lineNum}
                  className={isHighlighted ? "bg-[#1c2128]" : undefined}
                >
                  <span className="mr-4 inline-block w-8 select-none text-right text-[#636e7b]">
                    {lineNum}
                  </span>
                  {line}
                </div>
              );
            })}
          </pre>
        ) : item.blob_ref != null ? (
          <div className="flex h-full items-center justify-center">
            <p className="text-sm text-muted-foreground">
              Source exceeds inline preview limit.{" "}
              <span className="font-mono text-xs">{item.blob_ref}</span>
            </p>
          </div>
        ) : (
          <div className="flex h-full items-center justify-center">
            <p className="text-sm text-muted-foreground">No source available.</p>
          </div>
        )}
      </div>
    </div>
  );
}
