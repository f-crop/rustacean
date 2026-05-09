import { useMe, useRecentIngestions, useInvalidateRecentIngestions } from "@/api";
import { useEffect } from "react";
import { PageContainer } from "@/components/repos/PageContainer";
import { useEventStream } from "@/hooks/useEventStream";
import { SessionHistory } from "@/components/agent-execution/SessionHistory";
import { ExecutionStream } from "@/components/agent-execution/ExecutionStream";

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

  // useRecentIngestions fetches from the backend's ingestion-runs table,
  // which backs the "Agent Execution" view — each ingestion run corresponds
  // to an agent execution session.
  const recentSessions = useRecentIngestions(tenantId);
  const invalidateSessions = useInvalidateRecentIngestions();

  const { events, lastEventId, readyState } = useEventStream(`${apiBase}/v1/ingest/events`);

  useEffect(() => {
    const latestIngest = events
      .filter((e) => e.type === "ingest.status")
      .at(-1);
    if (!latestIngest) return;
    try {
      const parsed = JSON.parse(latestIngest.data) as { status?: string };
      if (parsed.status === "succeeded" || parsed.status === "done") {
        void invalidateSessions(tenantId);
      }
    } catch {
      // malformed event — ignore
    }
  }, [events, tenantId, invalidateSessions]);

  const sessions = recentSessions.data?.runs ?? [];

  return (
    <PageContainer>
      <header className="mb-6 flex flex-col gap-1">
        <h1 className="text-2xl font-semibold tracking-tight">
          Agent Execution
        </h1>
        <p className="text-sm text-muted-foreground">
          View execution sessions, live event streams, and trace details.
        </p>
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
            sessions={sessions}
            isLoading={recentSessions.isLoading}
            isError={recentSessions.isError}
            error={recentSessions.error}
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
    </PageContainer>
  );
}
