import { useState } from "react";
import { ChevronDown, ChevronRight, Box, FunctionSquare, Layers, Package, type LucideProps } from "lucide-react";
import type { ForwardRefExoticComponent, RefAttributes } from "react";
import type { components } from "@/api/generated/schema";
import { fqnToB64 } from "@/api/hooks/useCodeIntel";
import { cn } from "@/lib/utils";

type ModuleNodeItem = components["schemas"]["ModuleNodeItem"];
type LucideIcon = ForwardRefExoticComponent<Omit<LucideProps, "ref"> & RefAttributes<SVGSVGElement>>;

interface ModuleTreeProps {
  readonly tree: ModuleNodeItem;
  readonly selectedFqn: string | null;
  readonly onSelect: (fqn: string, fqnB64: string) => void;
}

export function ModuleTree({ tree, selectedFqn, onSelect }: ModuleTreeProps): JSX.Element {
  return (
    <nav aria-label="Module tree" className="h-full overflow-y-auto py-2 text-sm">
      <TreeNode node={tree} depth={0} selectedFqn={selectedFqn} onSelect={onSelect} />
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
}

function TreeNode({ node, depth, selectedFqn, onSelect }: TreeNodeProps): JSX.Element {
  const hasChildren = node.children.length > 0;
  const [expanded, setExpanded] = useState(depth < 2);

  const isModule = node.kind === "MOD";
  const isSelectable = !isModule || node.source != null;
  const isSelected = selectedFqn === node.fqn;
  const Icon = kindIcon(node.kind);

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
    if (e.key === "ArrowRight" && hasChildren && !expanded) {
      setExpanded(true);
    }
    if (e.key === "ArrowLeft" && hasChildren && expanded) {
      setExpanded(false);
    }
  };

  return (
    <div>
      <div
        role={isSelectable ? "treeitem" : "group"}
        aria-expanded={hasChildren ? expanded : undefined}
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
            {expanded ? (
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

      {hasChildren && expanded && (
        <div role="group">
          {node.children.map((child) => (
            <TreeNode
              key={child.fqn}
              node={child}
              depth={depth + 1}
              selectedFqn={selectedFqn}
              onSelect={onSelect}
            />
          ))}
        </div>
      )}
    </div>
  );
}
