// REQ-FE-02: shared route paths so navigation calls stay symbolic.
export const routes = {
  signup: "/signup",
  login: "/login",
  verifyEmail: "/verify-email",
  forgotPassword: "/forgot-password",
  resetPassword: "/reset-password",
  repos: "/repos",
  repoDetail: "/repos/$repoId",
  members: "/members",
  apiKeys: "/api-keys",
  ingestion: "/ingestion",
  codeWorkspace: "/repos/$repoId/code",
  activity: "/activity",
  agentExecution: "/agents/executions",
  agentSessionDetail: "/agents/executions/$sessionId",
  agentSessionReplay: "/agents/$sessionId",
  adminGithub: "/admin/github",
  trace: "/trace/$traceId",
  chat: "/chat",
} as const;

export type RoutePath = (typeof routes)[keyof typeof routes];
