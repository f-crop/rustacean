import { useState } from "react";
import {
  useCallers,
  useCallees,
  fqnToB64,
  type TraversalResponse,
  type TraversalNodeSchema,
} from "@/api/hooks/useCodeIntel";
import { formatApiError } from "@/lib/errors/api";
import { cn } from "@/lib/utils";

type RelationsTab = "callers" | "callees";

interface RelationsPanelProps {
  readonly repoId: string;
  readonly fqnB64: string | null;
  readonly onSelect: (fqn: string, fqnB64: string) => void;
}

export function RelationsPanel({ repoId, fqnB64, onSelect }: RelationsPanelProps): JSX.Element {
  const [tab, setTab] = useState<RelationsTab>("callers");

  const callers = useCallers(repoId, fqnB64 ?? "", { enabled: fqnB64 != null && fqnB64.length > 0 });
  const callees = useCallees(repoId, fqnB64 ?? "", { enabled: fqnB64 != null && fqnB64.length > 0 });

  if (!fqnB64) {
    return (
      <div
        className="flex h-full flex-col items-center justify-center gap-2 px-4 text-center"
        aria-label="Relations panel"
      >
        <p className="text-sm font-medium text-muted-foreground">No item selected</p>
        <p className="text-xs text-muted-foreground">
          Select a symbol from the module tree to explore its call graph.
        </p>
      </div>
    );
  }

  const active = tab === "callers" ? callers : callees;

  return (
    <div
      className="flex h-full flex-col"
      aria-label="Relations panel"
    >
      <div
        role="tablist"
        aria-label="Relations tabs"
        className="flex shrink-0 border-b border-border"
      >
        {(["callers", "callees"] as const).map((t) => (
          <button
            key={t}
            role="tab"
            type="button"
            aria-selected={tab === t}
            aria-controls={`relations-panel-${t}`}
            onClick={() => setTab(t)}
            className={cn(
              "flex-1 px-3 py-2 text-xs font-medium capitalize transition-colors",
              tab === t
                ? "border-b-2 border-primary text-foreground"
                : "text-muted-foreground hover:text-foreground",
            )}
          >
            {t}
          </button>
        ))}
      </div>

      <div
        id={`relations-panel-${tab}`}
        role="tabpanel"
        className="flex-1 overflow-y-auto"
      >
        {active.isLoading && (
          <p className="p-3 text-xs text-muted-foreground">Loading graph…</p>
        )}

        {active.isError && (
          <p className="p-3 text-xs text-destructive" role="alert">
            {formatApiError(active.error, "Could not load relations.")}
          </p>
        )}

        {active.data && (
          <TraversalGraph
            data={active.data}
            onSelect={onSelect}
            direction={tab}
          />
        )}
      </div>
    </div>
  );
}

interface TraversalGraphProps {
  readonly data: TraversalResponse;
  readonly onSelect: (fqn: string, fqnB64: string) => void;
  readonly direction: RelationsTab;
}

function TraversalGraph({ data, onSelect, direction }: TraversalGraphProps): JSX.Element {
  const related = data.nodes.filter((n) => n.fqn !== data.root.fqn);
  const label = direction === "callers" ? "callers" : "callees";

  return (
    <div className="py-1">
      <div className="border-b border-border px-3 py-2">
        <p className="text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
          Root
        </p>
        <p
          className="truncate font-mono text-xs font-semibold text-foreground"
          title={data.root.fqn}
        >
          {data.root.name ?? data.root.fqn}
        </p>
        {data.cycles_detected && (
          <p className="mt-1 text-[10px] text-amber-500">Cycle detected in graph</p>
        )}
      </div>

      <div className="px-3 py-2">
        <p className="mb-1 text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
          {label} ({related.length})
        </p>
      </div>

      {related.length === 0 ? (
        <div className="px-3 pb-3">
          <p className="text-xs text-muted-foreground">
            Call graph analysis is not yet available for this repository.
          </p>
          <p className="mt-1 text-xs text-muted-foreground/70">
            {label.charAt(0).toUpperCase() + label.slice(1)} will appear here once call graph
            extraction has been completed for this codebase.
          </p>
        </div>
      ) : (
        <ul
          aria-label={`${direction} list`}
          data-testid={`${direction}-list`}
        >
          {related.map((node) => (
            <NodeItem key={node.fqn} node={node} onSelect={onSelect} />
          ))}
        </ul>
      )}
    </div>
  );
}

interface NodeItemProps {
  readonly node: TraversalNodeSchema;
  readonly onSelect: (fqn: string, fqnB64: string) => void;
}

function NodeItem({ node, onSelect }: NodeItemProps): JSX.Element {
  return (
    <li>
      <button
        type="button"
        onClick={() => onSelect(node.fqn, fqnToB64(node.fqn))}
        aria-label={`Navigate to ${node.fqn}`}
        className={cn(
          "w-full px-3 py-2 text-left",
          "hover:bg-accent hover:text-accent-foreground",
          "focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring",
        )}
      >
        <p className="truncate font-mono text-xs font-medium text-foreground">
          {node.name ?? node.fqn}
        </p>
        {node.file_path && (
          <p className="mt-0.5 truncate font-mono text-[10px] text-muted-foreground">
            {node.file_path}
            {node.line != null && <span>:{node.line}</span>}
          </p>
        )}
      </button>
    </li>
  );
}
