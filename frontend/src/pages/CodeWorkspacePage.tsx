import { useParams, useSearch, useNavigate, Link } from "@tanstack/react-router";
import { useState } from "react";
import { useMe, useRepos } from "@/api";
import { useModuleTree, useItem, b64ToFqn } from "@/api/hooks/useCodeIntel";
import {
  ModuleTree,
  SourceViewer,
  SearchPanel,
  RelationsPanel,
} from "@/components/code-intel";
import { routes } from "@/lib/routes";
import { formatApiError } from "@/lib/errors/api";
import { cn } from "@/lib/utils";

export function CodeWorkspacePage(): JSX.Element {
  const { repoId } = useParams({ from: routes.codeWorkspace });
  const me = useMe({ retry: false });

  if (me.isLoading) {
    return (
      <WorkspaceShell>
        <p className="text-sm text-muted-foreground">Loading session…</p>
      </WorkspaceShell>
    );
  }
  if (me.isError || !me.data) {
    return (
      <WorkspaceShell>
        <p className="text-sm text-muted-foreground">You need to be signed in.</p>
        <Link to={routes.login} className="mt-2 inline-block text-sm text-primary hover:underline">
          Sign in →
        </Link>
      </WorkspaceShell>
    );
  }

  return <CodeWorkspaceInner repoId={repoId} tenantId={me.data.current_tenant.id} />;
}

interface WorkspaceShellProps {
  readonly children: React.ReactNode;
}

function WorkspaceShell({ children }: WorkspaceShellProps): JSX.Element {
  return (
    <div className="flex h-screen w-full items-center justify-center">
      {children}
    </div>
  );
}

type SideTab = "search" | "relations";

interface CodeWorkspaceInnerProps {
  readonly repoId: string;
  readonly tenantId: string;
}

function CodeWorkspaceInner({ repoId, tenantId }: CodeWorkspaceInnerProps): JSX.Element {
  const navigate = useNavigate();
  const { fqn: fqnB64 } = useSearch({ from: routes.codeWorkspace });

  const repos = useRepos(tenantId);
  const moduleTree = useModuleTree(repoId);
  const item = useItem(repoId, fqnB64 ?? "", { enabled: fqnB64 != null && fqnB64.length > 0 });

  const [sideTab, setSideTab] = useState<SideTab>("search");

  const repo = repos.data?.repos.find((r) => r.repo_id === repoId) ?? null;
  const selectedFqn = fqnB64 != null ? b64ToFqn(fqnB64) : null;

  const handleSelect = (_fqn: string, encodedB64: string) => {
    void navigate({
      to: routes.codeWorkspace,
      params: { repoId },
      search: { fqn: encodedB64 },
    });
  };

  const handleSearchSelect = (_fqn: string, encodedB64: string, resultRepoId: string) => {
    void navigate({
      to: routes.codeWorkspace,
      params: { repoId: resultRepoId },
      search: { fqn: encodedB64 },
    });
  };

  return (
    <div className="flex h-screen w-full flex-col overflow-hidden">
      <header className="flex shrink-0 items-center gap-3 border-b border-border bg-background px-4 py-2">
        <Link
          to={routes.repoDetail}
          params={{ repoId }}
          className="text-sm text-muted-foreground hover:text-foreground hover:underline"
        >
          ← {repo?.full_name ?? "Repository"}
        </Link>
        <span className="text-sm text-muted-foreground">/</span>
        <span className="text-sm font-medium">Code workspace</span>
      </header>

      <div className="flex min-h-0 flex-1">
        {/* Left: Module tree */}
        <aside
          aria-label="Module tree"
          className="flex w-56 shrink-0 flex-col overflow-hidden border-r border-border bg-muted/20"
        >
          <div className="shrink-0 border-b border-border px-3 py-2">
            <p className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
              Modules
            </p>
          </div>
          {moduleTree.isLoading ? (
            <p className="p-3 text-xs text-muted-foreground">Loading tree…</p>
          ) : moduleTree.isError ? (
            <p className="p-3 text-xs text-destructive">
              {formatApiError(moduleTree.error, "Could not load module tree.")}
            </p>
          ) : moduleTree.data ? (
            <ModuleTree
              tree={moduleTree.data.tree}
              selectedFqn={selectedFqn}
              onSelect={handleSelect}
            />
          ) : null}
        </aside>

        {/* Center: Source viewer */}
        <main className="flex flex-1 flex-col overflow-hidden" aria-label="Source viewer">
          {item.isLoading ? (
            <div className="flex h-full items-center justify-center">
              <p className="text-sm text-muted-foreground">Loading item…</p>
            </div>
          ) : item.isError ? (
            <div className="flex h-full items-center justify-center">
              <p className="text-sm text-destructive">
                {formatApiError(item.error, "Could not load item.")}
              </p>
            </div>
          ) : item.data ? (
            <SourceViewer item={item.data} />
          ) : (
            <div className="flex h-full items-center justify-center">
              <p className="text-sm text-muted-foreground">
                Select an item from the module tree to view its source.
              </p>
            </div>
          )}
        </main>

        {/* Right: Tabbed side panel */}
        <aside
          aria-label="Side panel"
          className="flex w-64 shrink-0 flex-col overflow-hidden border-l border-border"
        >
          <div
            role="tablist"
            aria-label="Side panel tabs"
            className="flex shrink-0 border-b border-border"
          >
            {(["search", "relations"] as const).map((tab) => (
              <button
                key={tab}
                role="tab"
                type="button"
                aria-selected={sideTab === tab}
                aria-controls={`side-panel-${tab}`}
                onClick={() => setSideTab(tab)}
                className={cn(
                  "flex-1 px-3 py-2 text-xs font-medium capitalize transition-colors",
                  sideTab === tab
                    ? "border-b-2 border-primary text-foreground"
                    : "text-muted-foreground hover:text-foreground",
                )}
              >
                {tab}
              </button>
            ))}
          </div>

          <div
            id="side-panel-search"
            role="tabpanel"
            aria-label="Search"
            className={cn("flex-1 overflow-hidden", sideTab !== "search" && "hidden")}
          >
            <SearchPanel onSelect={handleSearchSelect} repos={repos.data?.repos ?? []} />
          </div>
          <div
            id="side-panel-relations"
            role="tabpanel"
            aria-label="Relations"
            className={cn("flex-1 overflow-hidden", sideTab !== "relations" && "hidden")}
          >
            <RelationsPanel
              repoId={repoId}
              fqnB64={fqnB64 ?? null}
              onSelect={handleSelect}
            />
          </div>
        </aside>
      </div>
    </div>
  );
}
