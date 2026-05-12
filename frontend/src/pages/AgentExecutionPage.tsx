import { useState } from "react";
import { toast } from "sonner";
import { useMe, useAgentSessions, useCreateSession, useDeleteSession } from "@/api";
import type { CreateSessionFormValues } from "@/lib/validation/agentSessions";
import { formatApiError } from "@/lib/errors/api";
import { PageContainer } from "@/components/repos/PageContainer";
import { useEventStream } from "@/hooks/useEventStream";
import { SessionHistory } from "@/components/agent-execution/SessionHistory";
import { ExecutionStream } from "@/components/agent-execution/ExecutionStream";
import { CreateSessionDialog } from "@/components/agent-execution/CreateSessionDialog";

const SESSION_EVENT_TYPES = ["session.event"] as const;

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

  const sessions = useAgentSessions(tenantId);
  const createSession = useCreateSession(tenantId);
  const deleteSession = useDeleteSession(tenantId);

  const sseUrl = `${apiBase}/v1/agents/sessions/events`;
  const { events, lastEventId, readyState } = useEventStream(sseUrl, SESSION_EVENT_TYPES);

  const handleCreate = async (values: CreateSessionFormValues) => {
    const body: { runtime: string; initial_prompt?: string; workspace_path?: string | null } = {
      runtime: values.runtime,
    };
    if (values.initial_prompt) body.initial_prompt = values.initial_prompt;
    if (values.workspace_path) body.workspace_path = values.workspace_path;
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
            isDeleting={deletingId !== null}
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
