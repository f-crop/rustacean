// dev-watch probe 2026-06-17
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { ErrorBoundary } from "react-error-boundary";
import { Toaster } from "sonner";
import { AppErrorFallback } from "@/components/AppErrorFallback";
import { ThemeProvider } from "@/components/theme/ThemeProvider";
import { router } from "@/router";
import "@/index.css";
import "./styles/auth.css";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: (failureCount, error) => {
        const status = (error as { status?: number } | null)?.status;
        if (status && status >= 400 && status < 500) {
          return false;
        }
        return failureCount < 2;
      },
      staleTime: 30_000,
      refetchOnWindowFocus: false,
    },
    mutations: {
      retry: false,
    },
  },
});

const rootElement = document.getElementById("root");
if (!rootElement) {
  throw new Error("Root element #root not found");
}

createRoot(rootElement).render(
  <StrictMode>
    <ErrorBoundary FallbackComponent={AppErrorFallback}>
      <ThemeProvider defaultTheme="system">
        <QueryClientProvider client={queryClient}>
          <RouterProvider router={router} />
          <Toaster richColors position="top-right" />
        </QueryClientProvider>
      </ThemeProvider>
    </ErrorBoundary>
  </StrictMode>,
);
