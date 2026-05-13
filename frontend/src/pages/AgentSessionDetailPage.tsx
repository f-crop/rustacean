import { Link, useParams } from "@tanstack/react-router";
import { useMe, useSessionDetail } from "@/api";
import type { SessionDetail } from "@/api";
import { useEventStream } from "@/hooks/useEventStream";
import { ExecutionStream } from "@/components/agent-execution/ExecutionStream";
import { PageContainer } from "@/components/repos/PageContainer";
import { routes } from "@/lib/routes";
import { formatTimestamp } from "@/components/activity/utils";
import { formatApiError } from "@/lib/errors/api";

const AGENT_SESSION_EVENT_TYPES = ["session.event", "session.error"] as const;

export function AgentSessionDetailPage(): JSX.Element {
  const me = useMe({ retry: false });
  const { sessionId } = useParams({ strict: false });

  if (me.isLoading) {
    return (
      <PageContainer>
        <p className="text-sm text-muted-foreground">Loading…</p>
      </PageContainer>
    );
  }

  if (me.isError || !me.data) {
    return (
      <PageContainer>
        <p className="text-sm text-muted-foreground">Sign in to view session details.</p>
      </PageContainer>
    );
  }

  if (!sessionId) {
    return (
      <PageContainer>
        <p className="text-sm text-destructive">Invalid session URL.</p>
      </PageContainer>
    );
  }

  return (
    <AgentSessionDetailInner
      tenantId={me.data.current_tenant.id}
      sessionId={sessionId}
    />
  );
}

interface AgentSessionDetailInnerProps {
  readonly tenantId: string;
  readonly sessionId: string;
}

function AgentSessionDetailInner({ tenantId, sessionId }: AgentSessionDetailInnerProps): JSX.Element {
  const apiBase = import.meta.env.VITE_API_BASE_URL ?? "";
  const session = useSessionDetail(tenantId, sessionId, { refetchInterval: 5_000 });
  const { events, lastEventId, readyState } = useEventStream(
    `${apiBase}/v1/agents/sessions/${sessionId}/events`,
    AGENT_SESSION_EVENT_TYPES,
  );

  return (
    <PageContainer>
      <div className="mb-4">
        <Link
          to={routes.agentExecution}
          className="text-sm text-muted-foreground hover:text-foreground"
        >
          ← Back to sessions
        </Link>
      </div>

      <header className="mb-6">
        <h1 className="text-2xl font-semibold tracking-tight">Session Detail</h1>
        <p className="mt-1 font-mono text-xs text-muted-foreground" title={sessionId}>
          {sessionId}
        </p>
      </header>

      <div className="space-y-6">
        {session.isLoading ? (
          <p className="text-sm text-muted-foreground">Loading session…</p>
        ) : session.isError ? (
          <p className="text-sm text-destructive">
            {formatApiError(session.error, "Could not load session.")}
          </p>
        ) : session.data ? (
          <SessionMetadata session={session.data} />
        ) : null}

        <section aria-labelledby="stream-heading">
          <h2 id="stream-heading" className="mb-3 text-base font-semibold tracking-tight">
            Execution Stream
          </h2>
          <ExecutionStream events={events} lastEventId={lastEventId} readyState={readyState} />
        </section>
      </div>
    </PageContainer>
  );
}

const STATUS_LABEL_CLASS: Record<string, string> = {
  succeeded: "text-green-600 dark:text-green-400",
  completed: "text-green-600 dark:text-green-400",
  done: "text-green-600 dark:text-green-400",
  failed: "text-destructive",
  running: "text-blue-600 dark:text-blue-400",
  processing: "text-blue-600 dark:text-blue-400",
  pending: "text-muted-foreground",
  cancelled: "text-muted-foreground",
};

const RUNTIME_LABELS: Record<string, string> = {
  claude_code: "Claude Code",
  opencode: "OpenCode",
};

interface SessionMetadataProps {
  readonly session: SessionDetail;
}

function SessionMetadata({ session }: SessionMetadataProps): JSX.Element {
  const statusClass = STATUS_LABEL_CLASS[session.status] ?? "text-muted-foreground";
  const runtimeLabel = RUNTIME_LABELS[session.runtime_kind] ?? session.runtime_kind;

  return (
    <dl className="grid grid-cols-2 gap-x-6 gap-y-3 rounded-lg border border-border bg-card px-4 py-3 text-sm sm:grid-cols-3">
      <div>
        <dt className="text-xs text-muted-foreground">Status</dt>
        <dd className={`mt-0.5 font-medium capitalize ${statusClass}`}>{session.status}</dd>
      </div>
      <div>
        <dt className="text-xs text-muted-foreground">Runtime</dt>
        <dd className="mt-0.5">{runtimeLabel}</dd>
      </div>
      <div>
        <dt className="text-xs text-muted-foreground">Tokens</dt>
        <dd className="mt-0.5 tabular-nums">
          {session.tokens_used.toLocaleString()} / {session.token_budget.toLocaleString()}
        </dd>
      </div>
      <div>
        <dt className="text-xs text-muted-foreground">Created</dt>
        <dd className="mt-0.5">{formatTimestamp(session.created_at)}</dd>
      </div>
      {session.started_at ? (
        <div>
          <dt className="text-xs text-muted-foreground">Started</dt>
          <dd className="mt-0.5">{formatTimestamp(session.started_at)}</dd>
        </div>
      ) : null}
      {session.completed_at ? (
        <div>
          <dt className="text-xs text-muted-foreground">Completed</dt>
          <dd className="mt-0.5">{formatTimestamp(session.completed_at)}</dd>
        </div>
      ) : null}
      {session.exit_code != null ? (
        <div>
          <dt className="text-xs text-muted-foreground">Exit code</dt>
          <dd className="mt-0.5 font-mono">{session.exit_code}</dd>
        </div>
      ) : null}
      {session.failure_reason ? (
        <div className="col-span-2 sm:col-span-3">
          <dt className="text-xs text-muted-foreground">Failure reason</dt>
          <dd className="mt-0.5 text-destructive">{session.failure_reason}</dd>
        </div>
      ) : null}
      {session.input_prompt_preview ? (
        <div className="col-span-2 sm:col-span-3">
          <dt className="text-xs text-muted-foreground">Prompt preview</dt>
          <dd className="mt-0.5 text-muted-foreground">{session.input_prompt_preview}</dd>
        </div>
      ) : null}
    </dl>
  );
}
