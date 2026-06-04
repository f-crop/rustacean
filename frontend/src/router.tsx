import { Suspense } from "react";
import {
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  redirect,
} from "@tanstack/react-router";
import { z } from "zod";
import { AppShell, GlobalSuspenseFallback } from "@/components/AppShell";
import { ActivityPage } from "@/pages/ActivityPage";
import { TraceViewerPage } from "@/pages/TraceViewerPage";
import { ChatPage } from "@/pages/ChatPage";
import { AgentExecutionPage } from "@/pages/AgentExecutionPage";
import { AgentSessionDetailPage } from "@/pages/AgentSessionDetailPage";
import { SessionReplayPage } from "@/pages/SessionReplayPage";
import { AdminGithubPage } from "@/pages/AdminGithubPage";
import { ApiKeysPage } from "@/pages/ApiKeysPage";
import { CodeWorkspacePage } from "@/pages/CodeWorkspacePage";
import { ForgotPasswordPage } from "@/pages/ForgotPasswordPage";
import { LoginPage } from "@/pages/LoginPage";
import { MembersPage } from "@/pages/MembersPage";
import { ReposPage } from "@/pages/ReposPage";
import { RepoDetailPage } from "@/pages/RepoDetailPage";
import { ResetPasswordPage } from "@/pages/ResetPasswordPage";
import { SignupPage } from "@/pages/SignupPage";
import { VerifyEmailPage } from "@/pages/VerifyEmailPage";
import { routes } from "@/lib/routes";

const rootRoute = createRootRoute({
  component: () => (
    <AppShell>
      <Suspense fallback={<GlobalSuspenseFallback />}>
        <Outlet />
      </Suspense>
    </AppShell>
  ),
});

const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  beforeLoad: () => {
    throw redirect({ to: routes.login, replace: true });
  },
  component: () => null,
});

const signupRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: routes.signup,
  component: SignupPage,
});

const loginRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: routes.login,
  component: LoginPage,
});

const verifyEmailSearchSchema = z.object({
  token: z.string().optional(),
});

const verifyEmailRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: routes.verifyEmail,
  validateSearch: verifyEmailSearchSchema,
  component: VerifyEmailPage,
});

const forgotPasswordRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: routes.forgotPassword,
  component: ForgotPasswordPage,
});

const resetPasswordSearchSchema = z.object({
  token: z.string().optional(),
});

const resetPasswordRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: routes.resetPassword,
  validateSearch: resetPasswordSearchSchema,
  component: ResetPasswordPage,
});

const reposSearchSchema = z.object({
  install: z.enum(["success", "conflict"]).optional(),
  installation_uuid: z.uuid().optional(),
  account_login: z.string().optional(),
  reason: z.enum(["active"]).optional(),
});

const reposRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: routes.repos,
  validateSearch: reposSearchSchema,
  component: ReposPage,
});

const repoDetailRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: routes.repoDetail,
  component: RepoDetailPage,
});

const membersRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: routes.members,
  component: MembersPage,
});

const apiKeysRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: routes.apiKeys,
  component: ApiKeysPage,
});

const ingestionRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: routes.ingestion,
  beforeLoad: () => {
    throw redirect({ to: routes.activity, replace: true });
  },
  component: () => null,
});

const codeWorkspaceSearchSchema = z.object({
  fqn: z.string().optional(),
});

const codeWorkspaceRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: routes.codeWorkspace,
  validateSearch: codeWorkspaceSearchSchema,
  component: CodeWorkspacePage,
});

const activityRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: routes.activity,
  component: ActivityPage,
});

const agentExecutionRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: routes.agentExecution,
  component: AgentExecutionPage,
});

const agentSessionDetailRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: routes.agentSessionDetail,
  component: AgentSessionDetailPage,
});

const agentSessionReplayRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: routes.agentSessionReplay,
  component: SessionReplayPage,
});

const adminGithubSearchSchema = z.object({
  registered: z.boolean().optional(),
});

const adminGithubRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: routes.adminGithub,
  validateSearch: adminGithubSearchSchema,
  component: AdminGithubPage,
});

const traceSearchSchema = z.object({
  runId: z.string().optional(),
});

const traceRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: routes.trace,
  validateSearch: traceSearchSchema,
  component: TraceViewerPage,
});

const chatSearchSchema = z.object({
  sessionId: z.string().optional(),
});

const chatRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: routes.chat,
  validateSearch: chatSearchSchema,
  beforeLoad: () => {
    // Feature flag check: VITE_FEATURE_CHAT_PANEL must be "true" at build/run time.
    // When rb-feature-resolver (S5) ships this will be replaced with a per-tenant API check.
    if (import.meta.env.VITE_FEATURE_CHAT_PANEL !== "true") {
      throw redirect({ to: routes.repos, replace: true });
    }
  },
  component: ChatPage,
});

const catchAllRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "$",
  beforeLoad: () => {
    throw redirect({ to: routes.login, replace: true });
  },
  component: () => null,
});

const routeTree = rootRoute.addChildren([
  indexRoute,
  signupRoute,
  loginRoute,
  verifyEmailRoute,
  forgotPasswordRoute,
  resetPasswordRoute,
  reposRoute,
  repoDetailRoute,
  membersRoute,
  apiKeysRoute,
  ingestionRoute,
  codeWorkspaceRoute,
  activityRoute,
  agentExecutionRoute,
  agentSessionDetailRoute,
  agentSessionReplayRoute,
  adminGithubRoute,
  traceRoute,
  chatRoute,
  catchAllRoute,
]);

export const router = createRouter({
  routeTree,
  defaultPreload: "intent",
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
