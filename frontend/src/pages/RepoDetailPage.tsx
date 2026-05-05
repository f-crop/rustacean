import { useState } from "react";
import { Link, useParams } from "@tanstack/react-router";
import { toast } from "sonner";
import { useMe, useRepos, useTriggerIngest } from "@/api";
import { formatApiError } from "@/lib/errors/api";
import { routes } from "@/lib/routes";
import { StatusBadge } from "@/components/repos/StatusBadge";
import { PageContainer } from "@/components/repos/PageContainer";

export function RepoDetailPage(): JSX.Element {
  const { repoId } = useParams({ from: "/repos/$repoId" });
  const me = useMe({ retry: false });

  if (me.isLoading) {
    return (
      <PageContainer>
        <p className="text-sm text-muted-foreground">Loading session…</p>
      </PageContainer>
    );
  }

  if (me.isError || !me.data) {
    return (
      <PageContainer>
        <p className="text-sm text-muted-foreground">
          You need to be signed in.
        </p>
        <Link
          to={routes.login}
          className="mt-2 inline-block text-sm text-primary hover:underline"
        >
          Sign in →
        </Link>
      </PageContainer>
    );
  }

  return (
    <RepoDetailInner repoId={repoId} tenantId={me.data.current_tenant.id} />
  );
}

interface RepoDetailInnerProps {
  readonly repoId: string;
  readonly tenantId: string;
}

function RepoDetailInner({
  repoId,
  tenantId,
}: RepoDetailInnerProps): JSX.Element {
  const repos = useRepos(tenantId);
  const triggerIngest = useTriggerIngest(tenantId);
  const [ingestRunId, setIngestRunId] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const repo = repos.data?.repos.find((r) => r.repo_id === repoId) ?? null;

  const handleIngest = async () => {
    if (!repo) {
      return;
    }
    setBusy(true);
    try {
      const result = await triggerIngest.mutateAsync(repo.repo_id);
      setIngestRunId(result.run_id);
      toast.success("Ingestion run queued.");
    } catch (err) {
      toast.error(formatApiError(err, "Could not trigger ingestion."));
    } finally {
      setBusy(false);
    }
  };

  return (
    <PageContainer>
      <nav className="mb-4">
        <Link
          to={routes.repos}
          className="text-sm text-muted-foreground hover:text-foreground hover:underline"
        >
          ← Repositories
        </Link>
      </nav>

      {repos.isLoading ? (
        <p className="text-sm text-muted-foreground">Loading repository…</p>
      ) : repos.isError ? (
        <p className="text-sm text-destructive">
          {formatApiError(repos.error, "Could not load repository.")}
        </p>
      ) : !repo ? (
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">
            Repository not found
          </h1>
          <p className="mt-2 text-sm text-muted-foreground">
            This repository could not be found in your workspace.
          </p>
        </div>
      ) : (
        <div className="flex flex-col gap-6">
          <header className="flex flex-col gap-1">
            <div className="flex items-center gap-3">
              <h1 className="text-2xl font-semibold tracking-tight">
                {repo.full_name}
              </h1>
              <Link
                to={routes.codeWorkspace}
                params={{ repoId: repo.repo_id }}
                className="rounded-md border border-border px-2.5 py-1 text-xs font-medium text-muted-foreground hover:border-primary hover:text-primary"
              >
                Code workspace →
              </Link>
            </div>
            <StatusBadge status={repo.status} />
          </header>

          <section
            aria-labelledby="repo-details-heading"
            className="rounded-lg border border-border bg-card p-4"
          >
            <h2
              id="repo-details-heading"
              className="mb-3 text-sm font-medium"
            >
              Details
            </h2>
            <dl className="grid grid-cols-2 gap-x-4 gap-y-2 text-sm">
              <dt className="text-muted-foreground">Default branch</dt>
              <dd className="font-mono">{repo.default_branch}</dd>

              <dt className="text-muted-foreground">Repository ID</dt>
              <dd className="truncate font-mono text-xs">{repo.repo_id}</dd>

              <dt className="text-muted-foreground">Connected</dt>
              <dd>
                {new Date(repo.connected_at).toLocaleString()}
              </dd>
            </dl>
          </section>

          <section
            aria-labelledby="ingest-heading"
            className="rounded-lg border border-border bg-card p-4"
          >
            <h2 id="ingest-heading" className="mb-1 text-sm font-medium">
              Ingestion
            </h2>
            <p className="mb-4 text-xs text-muted-foreground">
              Trigger a full ingestion run to index the latest state of this
              repository.
            </p>

            {ingestRunId ? (
              <div className="rounded-md bg-muted px-3 py-2 text-sm">
                <p className="font-medium text-foreground">
                  Ingestion run queued
                </p>
                <p className="mt-0.5 font-mono text-xs text-muted-foreground">
                  Run ID: {ingestRunId}
                </p>
              </div>
            ) : (
              <button
                type="button"
                disabled={busy || repo.status !== "connected"}
                onClick={handleIngest}
                title={
                  repo.status !== "connected"
                    ? "Repository must be in connected status to trigger ingestion"
                    : undefined
                }
                className="rounded-md bg-primary px-3 py-1.5 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-60"
              >
                {busy ? "Queuing run…" : "Trigger ingestion"}
              </button>
            )}
          </section>
        </div>
      )}
    </PageContainer>
  );
}
