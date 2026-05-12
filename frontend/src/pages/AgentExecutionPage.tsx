import { useEffect, useState } from "react";
import { toast } from "sonner";
import { useQueryClient } from "@tanstack/react-query";
import { useMe, useAgentSessions, useCreateSession, useDeleteSession, agentSessionsQueryKey } from "@/api";
import type { CreateSessionRequest } from "@/api";
import type { CreateSessionFormValues } from "@/lib/validation/agentSessions";
import { formatApiError } from "@/lib/errors/api";
import { PageContainer } from "@/components/repos/PageContainer";
import { useEventStream } from "@/hooks/useEventStream";
import { SessionHistory } from "@/components/agent-execution/SessionHistory";
import { ExecutionStream } from "@/components/agent-execution/ExecutionStream";
import { CreateSessionDialog } from "@/components/agent-execution/CreateSessionDialog";

const INGEST_EVENT_TYPES = ["ingest.status"] as const;

export function AgentExecutionPage(): JSX.Element {
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
        <h1 className="text-2xl font-semibold tracking-tight">Agent Execution</h1>
        <p className="mt-2 text-sm text-muted-foreground">
          Sign in to view agent execution sessions.
        </p>
      </PageContainer>
    );
  }

  return <AgentExecutionInner tenantId={me.data.current_tenant.id} />;
}

interface AgentExecutionInnerProps {
  readonly tenantId: string;
}

function AgentExecutionInner({ tenantId }: AgentExecutionInnerProps): JSX.Element {
  const apiBase = import.meta.env.VITE_API_BASE_URL ?? "";
  const [showCreate, setShowCreate] = useState(false);
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const qc = useQueryClient();

  const sessions = useAgentSessions(tenantId);
  const createSession = useCreateSession(tenantId);
  const deleteSession = useDeleteSession(tenantId);

  // Use the ingest SSE endpoint — the global /v1/agents/sessions/events endpoint
  // does not exist; per-session events live at /v1/agents/sessions/{id}/events.
  // Keep the ingest stream for live activity and use it to invalidate the session list.
  const { events, lastEventId, readyState } = useEventStream(
    `${apiBase}/v1/ingest/events`,
    INGEST_EVENT_TYPES,
  );

  // Invalidate sessions when ingest events arrive (belt-and-suspenders with refetchInterval)
  useEffect(() => {
    const latest = events.filter((e) => e.type === "ingest.status").at(-1);
    if (!latest) return;
    try {
      const parsed = JSON.parse(latest.data) as { status?: string };
      if (parsed.status === "succeeded" || parsed.status === "done" || parsed.status === "failed") {
        void qc.invalidateQueries({ queryKey: agentSessionsQueryKey(tenantId) });
      }
    } catch {
      // malformed event — ignore
    }
  }, [events, tenantId, qc]);

  const handleCreate = async (values: CreateSessionFormValues) => {
    const body: CreateSessionRequest = {
      runtime: values.runtime,
      ...(values.initial_prompt ? { initial_prompt: values.initial_prompt } : {}),
      ...(values.workspace_path ? { workspace_path: values.workspace_path } : {}),
    };
    const result = await createSession.mutateAsync(body);
    toast.success(`Session ${result.session_id.slice(0, 8)}… created.`);
    setShowCreate(false);
  };

  const handleDelete = async (id: string) => {
    setDeletingId(id);
    try {
      await deleteSession.mutateAsync(id);
      toast.success("Session terminated.");
    } catch (err) {
      toast.error(formatApiError(err, "Could not terminate session."));
    } finally {
      setDeletingId(null);
    }
  };

  const sessionList = sessions.data?.sessions ?? [];

  return (
    <PageContainer>
      <header className="mb-6 flex items-start justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">
            Agent Execution
          </h1>
          <p className="mt-1 text-sm text-muted-foreground">
            View execution sessions, live event streams, and manage agents.
          </p>
        </div>
        <button
          type="button"
          onClick={() => setShowCreate(true)}
          className="shrink-0 rounded-md bg-primary px-3 py-1.5 text-sm font-medium text-primary-foreground shadow-sm hover:bg-primary/90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          New session
        </button>
      </header>

      <div className="space-y-8">
        <section aria-labelledby="sessions-heading">
          <h2
            id="sessions-heading"
            className="mb-3 text-base font-semibold tracking-tight"
          >
            Session History
          </h2>
          <SessionHistory
            sessions={sessionList}
            isLoading={sessions.isLoading}
            isError={sessions.isError}
            error={sessions.error}
            onDelete={handleDelete}
            deletingId={deletingId}
          />
        </section>

        <section aria-labelledby="stream-heading">
          <h2
            id="stream-heading"
            className="mb-3 text-base font-semibold tracking-tight"
          >
            Execution Stream
          </h2>
          <ExecutionStream events={events} lastEventId={lastEventId} readyState={readyState} />
        </section>
      </div>

      {showCreate ? (
        <CreateSessionDialog
          isPending={createSession.isPending}
          onSubmit={handleCreate}
          onClose={() => setShowCreate(false)}
        />
      ) : null}
    </PageContainer>
  );
}
