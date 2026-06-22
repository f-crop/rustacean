import { useState, useMemo, useRef } from "react";
import {
  ChevronDown,
  ChevronRight,
  Box,
  FunctionSquare,
  Layers,
  Package,
  Search,
  type LucideProps,
} from "lucide-react";
import type { ForwardRefExoticComponent, RefAttributes } from "react";
import type { components } from "@/api/generated/schema";
import { fqnToB64 } from "@/api/hooks/useCodeIntel";
import { cn } from "@/lib/utils";
import { filterTree } from "./module-tree-utils";

type ModuleNodeItem = components["schemas"]["ModuleNodeItem"];
type LucideIcon = ForwardRefExoticComponent<Omit<LucideProps, "ref"> & RefAttributes<SVGSVGElement>>;

interface ModuleTreeProps {
  readonly tree: ModuleNodeItem;
  readonly selectedFqn: string | null;
  readonly onSelect: (fqn: string, fqnB64: string) => void;
}

export function ModuleTree({ tree, selectedFqn, onSelect }: ModuleTreeProps): JSX.Element {
  const [searchQuery, setSearchQuery] = useState("");
  const searchInputRef = useRef<HTMLInputElement>(null);

  const { filteredNode, matchCount } = useMemo(() => {
    const q = searchQuery.trim();
    if (!q) return { filteredNode: tree, matchCount: 0 };
    return filterTree(tree, q);
  }, [tree, searchQuery]);

  const isSearchActive = searchQuery.trim().length > 0;

  const handleNavKeyDown = (e: React.KeyboardEvent) => {
    if ((e.metaKey || e.ctrlKey) && e.key === "k") {
      e.preventDefault();
      searchInputRef.current?.focus();
    }
  };

  return (
    <nav className="flex h-full flex-col text-sm" onKeyDown={handleNavKeyDown}>
      <div className="shrink-0 border-b border-border px-2 py-1.5">
        <div className="relative flex items-center">
          <Search className="pointer-events-none absolute left-2 h-3 w-3 shrink-0 text-muted-foreground" />
          <input
            ref={searchInputRef}
            type="search"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                setSearchQuery("");
                (e.currentTarget as HTMLInputElement).blur();
              }
            }}
            placeholder="Filter by path…"
            aria-label="Search module paths"
            className="w-full min-w-0 rounded-sm border border-input bg-background py-1 pl-6 pr-2 text-xs placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-ring"
          />
        </div>
        {isSearchActive && (
          <p
            className="mt-0.5 text-right text-[10px] text-muted-foreground"
            aria-live="polite"
            aria-atomic="true"
          >
            {matchCount} {matchCount === 1 ? "match" : "matches"}
          </p>
        )}
      </div>

      <div className="flex-1 overflow-y-auto py-2">
        {filteredNode === null ? (
          <p className="px-3 py-4 text-xs text-muted-foreground">No matches</p>
        ) : (
          <TreeNode
            node={filteredNode}
            depth={0}
            selectedFqn={selectedFqn}
            onSelect={onSelect}
            isSearchActive={isSearchActive}
          />
        )}
      </div>
    </nav>
  );
}

const KIND_ICONS: Record<string, LucideIcon> = {
  MOD: Layers,
  FN: FunctionSquare,
  STRUCT: Box,
  ENUM: Box,
  TRAIT: Package,
};

function kindIcon(kind: string): LucideIcon {
  return KIND_ICONS[kind] ?? Box;
}

function kindLabel(kind: string): string {
  const labels: Record<string, string> = {
    MOD: "module",
    FN: "function",
    STRUCT: "struct",
    ENUM: "enum",
    TRAIT: "trait",
    IMPL: "impl",
    TYPE: "type",
    CONST: "const",
    STATIC: "static",
    MACRO: "macro",
  };
  return labels[kind] ?? kind.toLowerCase();
}

interface TreeNodeProps {
  readonly node: ModuleNodeItem;
  readonly depth: number;
  readonly selectedFqn: string | null;
  readonly onSelect: (fqn: string, fqnB64: string) => void;
  readonly isSearchActive?: boolean;
}

function TreeNode({
  node,
  depth,
  selectedFqn,
  onSelect,
  isSearchActive = false,
}: TreeNodeProps): JSX.Element {
  const hasChildren = node.children.length > 0;
  const [expanded, setExpanded] = useState(depth < 2);

  const isModule = node.kind === "MOD";
  const isSelectable = !isModule || node.source != null;
  const isSelected = selectedFqn === node.fqn;
  const Icon = kindIcon(node.kind);

  // During search, auto-expand all nodes with children — they are ancestors of matches.
  // The local `expanded` state is not modified, so it restores naturally when search clears.
  const effectiveExpanded = (isSearchActive && hasChildren) || expanded;

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      if (isSelectable) {
        onSelect(node.fqn, fqnToB64(node.fqn));
      }
      if (hasChildren) {
        setExpanded((v) => !v);
      }
    }
    if (e.key === "ArrowRight" && hasChildren && !effectiveExpanded) {
      setExpanded(true);
    }
    if (e.key === "ArrowLeft" && hasChildren && effectiveExpanded) {
      setExpanded(false);
    }
  };

  return (
    <div>
      <div
        role={isSelectable ? "treeitem" : "group"}
        aria-expanded={hasChildren ? effectiveExpanded : undefined}
        aria-selected={isSelected}
        aria-label={`${node.name} — ${kindLabel(node.kind)}`}
        tabIndex={0}
        className={cn(
          "flex cursor-pointer items-center gap-1 rounded-sm px-2 py-0.5 outline-none",
          "hover:bg-accent hover:text-accent-foreground",
          "focus-visible:ring-1 focus-visible:ring-ring",
          isSelected && "bg-accent font-medium text-accent-foreground",
        )}
        style={{ paddingLeft: `${depth * 12 + 8}px` }}
        onClick={() => {
          if (isSelectable) {
            onSelect(node.fqn, fqnToB64(node.fqn));
          }
          if (hasChildren) {
            setExpanded((v) => !v);
          }
        }}
        onKeyDown={handleKeyDown}
      >
        {hasChildren ? (
          <span className="shrink-0 text-muted-foreground">
            {effectiveExpanded ? (
              <ChevronDown className="h-3 w-3" />
            ) : (
              <ChevronRight className="h-3 w-3" />
            )}
          </span>
        ) : (
          <span className="w-3 shrink-0" />
        )}
        <Icon className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
        <span className="truncate font-mono text-xs">{node.name}</span>
      </div>

      {hasChildren && effectiveExpanded && (
        <div role="group">
          {node.children.map((child) => (
            <TreeNode
              key={child.fqn}
              node={child}
              depth={depth + 1}
              selectedFqn={selectedFqn}
              onSelect={onSelect}
              isSearchActive={isSearchActive}
            />
          ))}
        </div>
      )}
    </div>
  );
}
