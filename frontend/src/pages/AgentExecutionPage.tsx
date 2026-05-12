import { useMe, useAgentSessions } from "@/api";
import { useState } from "react";
import { PageContainer } from "@/components/repos/PageContainer";
import { SessionHistory } from "@/components/agent-execution/SessionHistory";
import { CreateSessionDialog } from "@/components/agent-execution/CreateSessionDialog";

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

  return <AgentExecutionInner />;
}

function AgentExecutionInner(): JSX.Element {
  const [dialogOpen, setDialogOpen] = useState(false);

  const sessions = useAgentSessions({
    refetchInterval: 10_000,
  });

  const sessionList = sessions.data?.sessions ?? [];

  return (
    <PageContainer>
      <header className="mb-6 flex flex-col gap-1">
        <div className="flex items-center justify-between">
          <h1 className="text-2xl font-semibold tracking-tight">
            Agent Execution
          </h1>
          <button
            type="button"
            onClick={() => setDialogOpen(true)}
            className="rounded-md bg-primary px-3 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90"
          >
            New Session
          </button>
        </div>
        <p className="text-sm text-muted-foreground">
          View and manage agent execution sessions.
        </p>
      </header>

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
        />
      </section>

      {dialogOpen && (
        <CreateSessionDialog
          onClose={() => setDialogOpen(false)}
          onSuccess={() => {
            setDialogOpen(false);
            void sessions.refetch();
          }}
        />
      )}
    </PageContainer>
  );
}
