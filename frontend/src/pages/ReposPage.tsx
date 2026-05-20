import { useEffect, useRef, useState } from "react";
import { Link, useSearch } from "@tanstack/react-router";
import { toast } from "sonner";
import { useForm } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";
import {
  useMe,
  useRepos,
  useConnectRepo,
  useAvailableRepos,
  useGithubInstallUrl,
  type RepoItem,
  type AvailableRepo,
} from "@/api";
import { formatApiError } from "@/lib/errors/api";
import { routes } from "@/lib/routes";
import {
  connectRepoFormSchema,
  type ConnectRepoFormValues,
} from "@/lib/validation/repos";
import { StatusBadge } from "@/components/repos/StatusBadge";
import { PageContainer } from "@/components/repos/PageContainer";

// ---------------------------------------------------------------------------
// ReposPage — entry point
// ---------------------------------------------------------------------------

function isEmailNotVerifiedError(error: unknown): boolean {
  if (!error || typeof error !== "object") return false;
  const body = (error as { body?: { error?: string } }).body;
  return body?.error === "email_not_verified";
}

export function ReposPage(): JSX.Element {
  const me = useMe({ retry: false });
  const search = useSearch({ from: routes.repos });

  if (me.isLoading) {
    return (
      <PageContainer>
        <p className="text-sm text-muted-foreground">Loading session…</p>
      </PageContainer>
    );
  }

  if (me.isError || !me.data) {
    if (isEmailNotVerifiedError(me.error)) {
      return (
        <PageContainer>
          <h1 className="text-2xl font-semibold tracking-tight">Repositories</h1>
          <p className="mt-2 text-sm text-muted-foreground">
            {search.install === "success"
              ? "GitHub App installed — please verify your email to continue."
              : "Your email isn't verified yet. Please verify it to manage repositories."}
          </p>
          <Link
            to={routes.verifyEmail}
            className="mt-4 inline-block text-sm text-primary hover:underline"
          >
            Verify email →
          </Link>
        </PageContainer>
      );
    }

    return (
      <PageContainer>
        <h1 className="text-2xl font-semibold tracking-tight">Repositories</h1>
        <p className="mt-2 text-sm text-muted-foreground">
          You need to be signed in to manage repositories.
        </p>
        <Link
          to={routes.login}
          className="mt-4 inline-block text-sm text-primary hover:underline"
        >
          Sign in →
        </Link>
      </PageContainer>
    );
  }

  return <ReposPageInner tenantId={me.data.current_tenant.id} />;
}

// ---------------------------------------------------------------------------
// Inner page (tenant resolved)
// ---------------------------------------------------------------------------

interface ReposPageInnerProps {
  readonly tenantId: string;
}

function ReposPageInner({ tenantId }: ReposPageInnerProps): JSX.Element {
  const repos = useRepos(tenantId);
  const [showConnect, setShowConnect] = useState(false);
  const [dialogInstallId, setDialogInstallId] = useState<string | null>(null);
  const search = useSearch({ from: routes.repos });

  const connectedList: readonly RepoItem[] = repos.data?.repos ?? [];
  const installHandledRef = useRef(false);

  // Auto-open connect dialog when redirected back from GitHub App install.
  // We use window.history.replaceState instead of navigate() to clean the URL
  // without going through TanStack Router, which would remount this component
  // and reset the ref — causing a duplicate toast.
  useEffect(() => {
    if (!installHandledRef.current && search.install === "success" && search.installation_uuid) {
      installHandledRef.current = true;
      setDialogInstallId(search.installation_uuid);
      setShowConnect(true);
      toast.success(
        search.account_login
          ? `Installed on ${search.account_login}. Pick a repo to connect.`
          : "App installed. Pick a repo to connect.",
      );
      window.history.replaceState(null, "", routes.repos);
    } else if (!installHandledRef.current && search.install === "conflict") {
      installHandledRef.current = true;
      toast.error(
        "This GitHub account is already installed by another workspace. " +
        "Please contact your administrator or re-install the GitHub App on a different account.",
      );
      window.history.replaceState(null, "", routes.repos);
    }
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Derive the installation UUID from the first connected repo (if any) as
  // fallback for returning users who already have a connection.
  const knownInstallationId =
    dialogInstallId ?? connectedList[0]?.installation_id ?? null;

  return (
    <PageContainer>
      <header className="mb-6 flex items-start justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">Repositories</h1>
          <p className="mt-1 text-sm text-muted-foreground">
            Connected GitHub repositories for this workspace.
          </p>
        </div>
        <button
          type="button"
          onClick={() => setShowConnect(true)}
          className="shrink-0 rounded-md bg-primary px-3 py-1.5 text-sm font-medium text-primary-foreground shadow-sm hover:bg-primary/90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          Connect a repo
        </button>
      </header>

      {repos.isLoading ? (
        <p className="text-sm text-muted-foreground">Loading repositories…</p>
      ) : repos.isError ? (
        <p className="text-sm text-destructive">
          {formatApiError(repos.error, "Could not load repositories.")}
        </p>
      ) : connectedList.length === 0 ? (
        <EmptyState onConnect={() => setShowConnect(true)} />
      ) : (
        <RepoList repos={connectedList} />
      )}

      {showConnect ? (
        <ConnectRepoDialog
          tenantId={tenantId}
          installationId={knownInstallationId}
          onClose={() => setShowConnect(false)}
          onSuccess={() => setShowConnect(false)}
        />
      ) : null}
    </PageContainer>
  );
}

// ---------------------------------------------------------------------------
// Repo list
// ---------------------------------------------------------------------------

function RepoList({ repos }: { readonly repos: readonly RepoItem[] }): JSX.Element {
  return (
    <ul className="divide-y divide-border rounded-lg border border-border bg-card">
      {repos.map((repo) => (
        <RepoRow key={repo.repo_id} repo={repo} />
      ))}
    </ul>
  );
}

function RepoRow({ repo }: { readonly repo: RepoItem }): JSX.Element {
  const connectedAt = new Date(repo.connected_at);
  const relativeDate = Number.isNaN(connectedAt.getTime())
    ? "—"
    : connectedAt.toLocaleDateString();

  return (
    <li className="flex flex-col gap-2 px-4 py-3 sm:flex-row sm:items-center sm:justify-between">
      <div className="flex flex-col gap-0.5">
        <span className="text-sm font-medium">{repo.full_name}</span>
        <span className="text-xs text-muted-foreground">
          Branch: <span className="font-mono">{repo.default_branch}</span>
          {" · "}
          Connected {relativeDate}
        </span>
      </div>
      <div className="flex items-center gap-2">
        <StatusBadge status={repo.status} />
        <Link
          to="/repos/$repoId"
          params={{ repoId: repo.repo_id }}
          className="rounded-md border border-border px-2 py-1 text-xs font-medium text-foreground hover:bg-accent hover:text-accent-foreground"
        >
          View
        </Link>
      </div>
    </li>
  );
}

// ---------------------------------------------------------------------------
// Empty state
// ---------------------------------------------------------------------------

function EmptyState({ onConnect }: { readonly onConnect: () => void }): JSX.Element {
  return (
    <div className="flex flex-col items-center rounded-lg border border-dashed border-border py-12 text-center">
      <p className="text-sm font-medium text-foreground">No repositories connected</p>
      <p className="mt-1 text-xs text-muted-foreground">
        Connect a GitHub repository to start indexing your codebase.
      </p>
      <button
        type="button"
        onClick={onConnect}
        className="mt-4 rounded-md bg-primary px-3 py-1.5 text-sm font-medium text-primary-foreground hover:bg-primary/90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        Connect your first repo
      </button>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Connect repo dialog — focus trap + Escape
// ---------------------------------------------------------------------------

const FOCUSABLE_SELECTOR =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

function getFocusableElements(container: HTMLElement | null): HTMLElement[] {
  if (!container) return [];
  return Array.from(container.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR));
}

type ConnectStep = "install" | "pick";

interface ConnectRepoDialogProps {
  readonly tenantId: string;
  readonly installationId: string | null;
  readonly onClose: () => void;
  readonly onSuccess: () => void;
}

function ConnectRepoDialog({
  tenantId,
  installationId,
  onClose,
  onSuccess,
}: ConnectRepoDialogProps): JSX.Element {
  const step: ConnectStep = installationId ? "pick" : "install";
  const resolvedInstallId = installationId ?? "";

  const dialogRef = useRef<HTMLDivElement>(null);
  const previousFocusRef = useRef<HTMLElement | null>(null);

  // Save trigger element; focus first focusable on mount; restore on unmount
  useEffect(() => {
    previousFocusRef.current = document.activeElement as HTMLElement | null;
    const focusables = getFocusableElements(dialogRef.current);
    focusables[0]?.focus();
    return () => {
      previousFocusRef.current?.focus();
    };
  }, []);

  // Escape key closes dialog; Tab/Shift+Tab cycles within dialog
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        onClose();
        return;
      }
      if (e.key === "Tab") {
        const focusables = getFocusableElements(dialogRef.current);
        if (focusables.length === 0) return;
        const first = focusables[0];
        const last = focusables[focusables.length - 1];
        if (!first || !last) return;
        if (e.shiftKey) {
          if (document.activeElement === first) {
            e.preventDefault();
            last.focus();
          }
        } else {
          if (document.activeElement === last) {
            e.preventDefault();
            first.focus();
          }
        }
      }
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

  return (
    <div
      ref={dialogRef}
      role="dialog"
      aria-modal="true"
      aria-labelledby="connect-repo-title"
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
      onClick={(e) => {
        if (e.target === e.currentTarget) {
          onClose();
        }
      }}
    >
      <div className="flex w-full max-w-lg flex-col gap-4 rounded-lg border border-border bg-background p-6 shadow-xl">
        <div className="flex items-start justify-between">
          <h2
            id="connect-repo-title"
            className="text-lg font-semibold tracking-tight"
          >
            Connect a repository
          </h2>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close"
            className="rounded-md p-1 text-muted-foreground hover:bg-accent hover:text-accent-foreground"
          >
            ✕
          </button>
        </div>

        {step === "install" ? (
          <InstallStep />
        ) : (
          <PickRepoStep
            tenantId={tenantId}
            installationUuid={resolvedInstallId}
            onSuccess={onSuccess}
          />
        )}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Step 1 — Install GitHub App
// ---------------------------------------------------------------------------

function InstallStep(): JSX.Element {
  const installUrl = useGithubInstallUrl();

  const handleInstall = async () => {
    try {
      const result = await installUrl.mutateAsync();
      window.location.assign(result.url);
    } catch (err) {
      toast.error(
        isEmailNotVerifiedError(err)
          ? "Please verify your email before installing the GitHub App."
          : formatApiError(err, "Could not generate install link."),
      );
    }
  };

  return (
    <div className="flex flex-col gap-4">
      <p className="text-sm text-muted-foreground">
        First, install the GitHub App on the organization or account that owns
        the repositories you want to connect. You&apos;ll be redirected back
        here automatically after installing.
      </p>
      <button
        type="button"
        disabled={installUrl.isPending}
        onClick={handleInstall}
        className="rounded-md bg-primary px-3 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-60"
      >
        {installUrl.isPending ? "Generating link…" : "Install GitHub App →"}
      </button>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Step 2 — Pick repo from available list (with field-level validation)
// ---------------------------------------------------------------------------

function AvailableReposError({ error }: { readonly error: unknown }): JSX.Element {
  const installUrl = useGithubInstallUrl();
  const status = (error as { status?: number } | null)?.status;

  const handleReinstall = async () => {
    try {
      const result = await installUrl.mutateAsync();
      window.location.assign(result.url);
    } catch (err) {
      toast.error(formatApiError(err, "Could not generate install link."));
    }
  };

  if (status === 404 || status === 409) {
    return (
      <div className="flex flex-col gap-3">
        <p className="text-sm text-destructive">
          {status === 409
            ? "This installation belongs to a different GitHub App. Re-install the active App to continue."
            : "This GitHub installation is not accessible from your workspace. It may be linked to a different account. Re-install the GitHub App to connect repositories for this workspace."}
        </p>
        <button
          type="button"
          disabled={installUrl.isPending}
          onClick={handleReinstall}
          className="self-start rounded-md bg-primary px-3 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-60"
        >
          {installUrl.isPending ? "Generating link…" : "Re-install GitHub App →"}
        </button>
      </div>
    );
  }

  return (
    <p className="text-sm text-destructive">
      {formatApiError(error, "Could not load repositories.")}
    </p>
  );
}

interface PickRepoStepProps {
  readonly tenantId: string;
  readonly installationUuid: string;
  readonly onSuccess: () => void;
}

function PickRepoStep({
  tenantId,
  installationUuid,
  onSuccess,
}: PickRepoStepProps): JSX.Element {
  const available = useAvailableRepos(installationUuid, 1, {
    enabled: installationUuid.length > 0,
  });
  const connect = useConnectRepo(tenantId);
  const [busy, setBusy] = useState(false);

  const {
    handleSubmit,
    setValue,
    watch,
    formState: { errors },
  } = useForm<ConnectRepoFormValues>({
    resolver: zodResolver(connectRepoFormSchema),
  });

  const selectedRepoId = watch("github_repo_id");

  const handleSelectRepo = (repo: AvailableRepo) => {
    setValue("github_repo_id", repo.id, { shouldValidate: true });
    setValue("default_branch", repo.default_branch || undefined);
  };

  const onSubmit = handleSubmit(async (values) => {
    setBusy(true);
    try {
      const result = await connect.mutateAsync({
        installation_id: installationUuid,
        github_repo_id: values.github_repo_id,
        default_branch: values.default_branch ?? null,
      });
      toast.success(`Connected ${result.full_name}.`);
      onSuccess();
    } catch (err) {
      toast.error(formatApiError(err, "Could not connect repository."));
    } finally {
      setBusy(false);
    }
  });

  return (
    <form onSubmit={onSubmit} className="flex flex-col gap-4">
      {/* Available repos list */}
      {installationUuid.length === 0 ? (
        <p className="rounded-md bg-muted px-3 py-2 text-xs text-muted-foreground">
          Return here after installing the GitHub App to see available repositories.
        </p>
      ) : available.isLoading ? (
        <p className="text-sm text-muted-foreground">Loading available repos…</p>
      ) : available.isError ? (
        <AvailableReposError error={available.error} />
      ) : !available.data || available.data.repositories.length === 0 ? (
        <p className="text-sm text-muted-foreground">No repositories accessible.</p>
      ) : (
        <div className="flex flex-col gap-1">
          <p className="text-xs font-medium text-foreground">
            Select a repository
          </p>
          {errors.github_repo_id ? (
            <p className="text-xs text-destructive" role="alert">
              {errors.github_repo_id.message}
            </p>
          ) : null}
          <ul className="max-h-48 divide-y divide-border overflow-y-auto rounded-md border border-border bg-card">
            {available.data.repositories.map((repo) => (
              <li key={repo.id}>
                <button
                  type="button"
                  onClick={() => handleSelectRepo(repo)}
                  className={`w-full px-3 py-2 text-left text-sm hover:bg-accent hover:text-accent-foreground ${
                    selectedRepoId === repo.id
                      ? "bg-primary/10 font-medium"
                      : ""
                  }`}
                >
                  <span className="block font-medium">{repo.full_name}</span>
                  <span className="block text-xs text-muted-foreground">
                    {repo.private ? "Private" : "Public"}
                    {" · "}
                    {repo.default_branch}
                    {repo.archived ? " · archived" : ""}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}

      <button
        type="submit"
        disabled={busy}
        className="rounded-md bg-primary px-3 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-60"
      >
        {busy ? "Connecting…" : "Connect repository"}
      </button>
    </form>
  );
}
