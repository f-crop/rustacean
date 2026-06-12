import { Link } from "@tanstack/react-router";
import { ThemeToggle } from "@/components/theme/ThemeToggle";
import { routes } from "@/lib/routes";
import { useChatFeatureEnabled } from "@/hooks/useChatFeatureEnabled";
import type { ReactNode } from "react";

interface AppShellProps {
  children: ReactNode;
}

export function AppShell({ children }: AppShellProps): JSX.Element {
  const chatEnabled = useChatFeatureEnabled();

  return (
    <div className="flex h-screen flex-col overflow-hidden bg-background text-foreground">
      <header className="border-b border-border bg-background/80 backdrop-blur supports-[backdrop-filter]:bg-background/60">
        <div className="container flex h-14 items-center justify-between gap-4">
          <Link
            to={routes.repos}
            className="text-base font-semibold tracking-tight hover:text-primary"
          >
            Rustacean
          </Link>
          <nav
            aria-label="Primary"
            className="hidden items-center gap-1 sm:flex"
          >
            <Link
              to={routes.repos}
              className="rounded-md px-3 py-1.5 text-sm text-muted-foreground hover:bg-accent hover:text-accent-foreground aria-[current=page]:bg-accent aria-[current=page]:text-foreground aria-[current=page]:font-medium"
            >
              Repos
            </Link>
            <Link
              to={routes.members}
              className="rounded-md px-3 py-1.5 text-sm text-muted-foreground hover:bg-accent hover:text-accent-foreground aria-[current=page]:bg-accent aria-[current=page]:text-foreground aria-[current=page]:font-medium"
            >
              Members
            </Link>
            <Link
              to={routes.apiKeys}
              className="rounded-md px-3 py-1.5 text-sm text-muted-foreground hover:bg-accent hover:text-accent-foreground aria-[current=page]:bg-accent aria-[current=page]:text-foreground aria-[current=page]:font-medium"
            >
              API keys
            </Link>
            <Link
              to={routes.agentExecution}
              className="rounded-md px-3 py-1.5 text-sm text-muted-foreground hover:bg-accent hover:text-accent-foreground aria-[current=page]:bg-accent aria-[current=page]:text-foreground aria-[current=page]:font-medium"
            >
              Agents
            </Link>
            <Link
              to={routes.activity}
              className="rounded-md px-3 py-1.5 text-sm text-muted-foreground hover:bg-accent hover:text-accent-foreground aria-[current=page]:bg-accent aria-[current=page]:text-foreground aria-[current=page]:font-medium"
            >
              Activity
            </Link>
            <Link
              to={routes.adminGithub}
              className="rounded-md px-3 py-1.5 text-sm text-muted-foreground hover:bg-accent hover:text-accent-foreground aria-[current=page]:bg-accent aria-[current=page]:text-foreground aria-[current=page]:font-medium"
            >
              Admin
            </Link>
            {chatEnabled && (
              <Link
                to={routes.chat}
                className="rounded-md px-3 py-1.5 text-sm text-muted-foreground hover:bg-accent hover:text-accent-foreground aria-[current=page]:bg-accent aria-[current=page]:text-foreground aria-[current=page]:font-medium"
              >
                Chat
              </Link>
            )}
          </nav>
          <div className="flex items-center gap-2">
            <ThemeToggle />
          </div>
        </div>
      </header>
      <main className="flex-1 min-h-0 overflow-auto">{children}</main>
      <footer className="border-t border-border py-4">
        <div className="container text-xs text-muted-foreground">
          Rustacean control plane
        </div>
      </footer>
    </div>
  );
}

export function GlobalSuspenseFallback(): JSX.Element {
  return (
    <div
      role="status"
      aria-live="polite"
      className="flex min-h-screen items-center justify-center bg-background"
    >
      <div className="flex items-center gap-3 text-sm text-muted-foreground">
        <span className="h-3 w-3 animate-pulse rounded-full bg-muted-foreground/60" />
        Loading…
      </div>
    </div>
  );
}
