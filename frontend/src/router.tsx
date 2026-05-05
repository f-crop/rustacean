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
import { ApiKeysPage } from "@/pages/ApiKeysPage";
import { CodeWorkspacePage } from "@/pages/CodeWorkspacePage";
import { ForgotPasswordPage } from "@/pages/ForgotPasswordPage";
import { IngestionTheatre } from "@/pages/IngestionTheatre";
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
  install: z.enum(["success"]).optional(),
  installation_uuid: z.uuid().optional(),
  account_login: z.string().optional(),
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
  component: IngestionTheatre,
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
