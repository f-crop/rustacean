import { useState } from "react";
import { Link, useSearch } from "@tanstack/react-router";
import { toast } from "sonner";
import {
  useGithubAppStatus,
  useGithubAppManifest,
  useMe,
} from "@/api";
import { formatApiError } from "@/lib/errors/api";
import { routes } from "@/lib/routes";

function formatTimestamp(value: string | null | undefined): string {
  if (!value) return "\u2014";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "\u2014";
  return date.toLocaleString();
}

function sourceLabel(source: string): string {
  switch (source) {
    case "db":
      return "Database (manifest flow)";
    case "env":
      return "Environment variable (legacy)";
    case "none":
      return "Not configured";
    default:
      return source;
  }
}

export function AdminGithubPage(): JSX.Element {
  const me = useMe({ retry: false });

  if (me.isLoading) {
    return (
      <PageContainer>
        <p className="text-sm text-muted-foreground">
          Loading your session&hellip;
        </p>
      </PageContainer>
    );
  }

  if (me.isError || !me.data) {
    return (
      <PageContainer>
        <h1 className="text-2xl font-semibold tracking-tight">
          GitHub App configuration
        </h1>
        <p className="mt-2 text-sm text-muted-foreground">
          You need to be signed in to access admin settings.
        </p>
        <Link
          to={routes.login}
          className="mt-4 inline-block text-sm text-primary hover:underline"
        >
          Sign in &rarr;
        </Link>
      </PageContainer>
    );
  }

  return <AdminGithubPageInner />;
}

function AdminGithubPageInner(): JSX.Element {
  const search = useSearch({ strict: false }) as { registered?: string };
  const [showRegisteredBanner, setShowRegisteredBanner] = useState(
    search.registered === "true",
  );

  const status = useGithubAppStatus({
    retry: shouldRetryAppStatus,
  });
  const manifest = useGithubAppManifest();

  const isForbidden = "status" in (status.error ?? {}) && (status.error as { status: number }).status === 403;

  const onInitiate = async () => {
    try {
      const result = await manifest.mutateAsync({ name: null });
      const url = new URL(result.redirect_url);
      const manifestJson = url.searchParams.get('manifest') ?? '';
      url.searchParams.delete('manifest');

      const form = document.createElement('form');
      form.method = 'POST';
      form.action = url.toString();

      const input = document.createElement('input');
      input.type = 'hidden';
      input.name = 'manifest';
      input.value = manifestJson;
      form.appendChild(input);

      document.body.appendChild(form);
      form.submit();
    } catch (error) {
      toast.error(formatApiError(error, "Could not initiate manifest registration."));
    }
  };

  return (
    <PageContainer>
      <header className="mb-6 flex flex-col gap-1">
        <h1 className="text-2xl font-semibold tracking-tight">
          GitHub App configuration
        </h1>
        <p className="text-sm text-muted-foreground">
          Register and manage the GitHub App used for repository integration.
          Only platform admins can access this page.
        </p>
      </header>

      {showRegisteredBanner ? (
        <section
          role="alert"
          aria-live="polite"
          className="mb-6 rounded-lg border border-emerald-500/60 bg-emerald-50 p-4 text-emerald-950 shadow-sm dark:bg-emerald-900/20 dark:text-emerald-100"
        >
          <div className="flex items-start justify-between gap-3">
            <div>
              <h2 className="text-sm font-semibold">
                GitHub App registered successfully
              </h2>
              <p className="mt-1 text-xs">
                The new GitHub App credentials have been stored and the app
                loader has been hot-swapped. You can verify the status below.
              </p>
            </div>
            <button
              type="button"
              onClick={() => setShowRegisteredBanner(false)}
              className="shrink-0 rounded-md border border-emerald-500/40 px-2 py-1 text-xs font-medium hover:bg-emerald-100 dark:hover:bg-emerald-900/40"
            >
              Dismiss
            </button>
          </div>
        </section>
      ) : null}

      {isForbidden ? (
        <section className="rounded-lg border border-border bg-card p-4">
          <h2 className="text-sm font-medium">Access denied</h2>
          <p className="mt-1 text-sm text-muted-foreground">
            You are not a platform admin. Only platform administrators can
            manage the GitHub App configuration.
          </p>
        </section>
      ) : status.isLoading ? (
        <p className="text-sm text-muted-foreground">
          Loading app status&hellip;
        </p>
      ) : status.isError ? (
        <p className="text-sm text-destructive">
          {formatApiError(status.error, "Could not load GitHub App status.")}
        </p>
      ) : status.data ? (
        <>
          <StatusCard configured={status.data.configured} source={status.data.source} />
          <DetailsCard data={status.data} />

          <section
            aria-labelledby="manifest-heading"
            className="mt-6 rounded-lg border border-border bg-card p-4"
          >
            <h2 id="manifest-heading" className="text-sm font-medium">
              Register a new GitHub App
            </h2>
            <p className="mt-1 text-xs text-muted-foreground">
              This initiates the GitHub App Manifest flow. You will be
              redirected to GitHub to confirm the app creation. The new app
              will replace any existing configuration.
            </p>
            <div className="mt-3">
              <button
                type="button"
                disabled={manifest.isPending}
                onClick={onInitiate}
                className="rounded-md border border-primary bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-50"
              >
                {manifest.isPending
                  ? "Redirecting\u2026"
                  : "Register via GitHub"}
              </button>
            </div>
            {manifest.isError ? (
              <p className="mt-2 text-xs text-destructive">
                {formatApiError(
                  manifest.error,
                  "Could not initiate manifest registration.",
                )}
              </p>
            ) : null}
          </section>
        </>
      ) : null}
    </PageContainer>
  );
}

interface StatusCardProps {
  readonly configured: boolean;
  readonly source: string;
}

function StatusCard({ configured, source }: StatusCardProps): JSX.Element {
  const indicator = configured ? (
    <span className="inline-flex items-center gap-1.5 rounded-full bg-emerald-100 px-2.5 py-0.5 text-xs font-medium text-emerald-800 dark:bg-emerald-900/30 dark:text-emerald-300">
      <span className="h-1.5 w-1.5 rounded-full bg-emerald-500" />
      Configured
    </span>
  ) : (
    <span className="inline-flex items-center gap-1.5 rounded-full bg-amber-100 px-2.5 py-0.5 text-xs font-medium text-amber-800 dark:bg-amber-900/30 dark:text-amber-300">
      <span className="h-1.5 w-1.5 rounded-full bg-amber-500" />
      Not configured
    </span>
  );

  return (
    <section className="mb-4 rounded-lg border border-border bg-card p-4">
      <div className="flex items-center gap-3">
        <h2 className="text-sm font-medium">Status</h2>
        {indicator}
      </div>
      <p className="mt-1 text-xs text-muted-foreground">
        Source: {sourceLabel(source)}
      </p>
    </section>
  );
}

interface DetailsCardProps {
  readonly data: {
    readonly app_id?: number | null;
    readonly configured: boolean;
    readonly installed_at?: string | null;
    readonly installed_by?: string | null;
    readonly slug?: string | null;
    readonly source: string;
  };
}

function DetailsCard({ data }: DetailsCardProps): JSX.Element {
  if (!data.configured) return <></>;

  const rows: Array<{ label: string; value: string }> = [
    { label: "App ID", value: data.app_id != null ? String(data.app_id) : "\u2014" },
    { label: "Slug", value: data.slug ?? "\u2014" },
    { label: "Installed at", value: formatTimestamp(data.installed_at ?? null) },
    { label: "Installed by", value: data.installed_by ?? "\u2014" },
  ];

  return (
    <section className="mb-4 rounded-lg border border-border bg-card p-4">
      <h2 className="mb-3 text-sm font-medium">App details</h2>
      <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-2 text-sm">
        {rows.map((row) => (
          <span key={row.label}>
            <dt className="text-muted-foreground">{row.label}</dt>
            <dd className="font-mono text-xs">{row.value}</dd>
          </span>
        ))}
      </dl>
    </section>
  );
}

function PageContainer({
  children,
}: {
  readonly children: React.ReactNode;
}): JSX.Element {
  return <div className="container max-w-3xl py-8">{children}</div>;
}

function shouldRetryAppStatus(failureCount: number, error: unknown): boolean {
  if (
    error !== null &&
    typeof error === "object" &&
    "status" in error &&
    (error as { status: number }).status === 403
  ) {
    return false;
  }
  return failureCount < 3;
}
