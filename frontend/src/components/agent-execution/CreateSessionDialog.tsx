import { useEffect, useRef, useState } from "react";
import { useForm } from "react-hook-form";
import { z } from "zod";
import { zodResolver } from "@hookform/resolvers/zod";
import { useCreateSession } from "@/api";
import { formatApiError } from "@/lib/errors/api";

// ---------------------------------------------------------------------------
// Focus-trap utilities (same pattern as ConnectRepoDialog in ReposPage)
// ---------------------------------------------------------------------------

const FOCUSABLE_SELECTOR =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

function getFocusableElements(container: HTMLElement | null): HTMLElement[] {
  if (!container) return [];
  return Array.from(container.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR));
}

// ---------------------------------------------------------------------------
// Form schema
// ---------------------------------------------------------------------------

const RUNTIME_OPTIONS = ["claude_code", "opencode", "pi"] as const;

const createSessionFormSchema = z.object({
  runtime: z.enum(RUNTIME_OPTIONS),
  initial_prompt: z.string().min(1, "Prompt is required").max(4096),
  workspace_path: z.string().max(512).optional(),
});

type CreateSessionFormValues = z.infer<typeof createSessionFormSchema>;

// ---------------------------------------------------------------------------
// Dialog component
// ---------------------------------------------------------------------------

interface CreateSessionDialogProps {
  readonly onClose: () => void;
  readonly onSuccess: () => void;
}

export function CreateSessionDialog({
  onClose,
  onSuccess,
}: CreateSessionDialogProps): JSX.Element {
  const createSession = useCreateSession();
  const [submitError, setSubmitError] = useState<string | null>(null);

  const dialogRef = useRef<HTMLDivElement>(null);
  const previousFocusRef = useRef<HTMLElement | null>(null);

  const {
    register,
    handleSubmit,
    formState: { errors, isSubmitting },
  } = useForm<CreateSessionFormValues>({
    resolver: zodResolver(createSessionFormSchema),
    defaultValues: {
      runtime: "claude_code",
      initial_prompt: "",
      workspace_path: "",
    },
  });

  // Focus trap + Escape — same pattern as ConnectRepoDialog
  useEffect(() => {
    previousFocusRef.current = document.activeElement as HTMLElement | null;
    const focusables = getFocusableElements(dialogRef.current);
    focusables[0]?.focus();
    return () => {
      previousFocusRef.current?.focus();
    };
  }, []);

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        onClose();
        return;
      }
      if (e.key === "Tab") {
        const focusables = getFocusableElements(dialogRef.current);
        if (focusables.length === 0) return;
        const first = focusables[0];
        const last = focusables[focusables.length - 1];
        if (!first || !last) return;
        if (e.shiftKey) {
          if (document.activeElement === first) {
            e.preventDefault();
            last.focus();
          }
        } else {
          if (document.activeElement === last) {
            e.preventDefault();
            first.focus();
          }
        }
      }
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

  const onSubmit = handleSubmit(async (values) => {
    setSubmitError(null);
    try {
      await createSession.mutateAsync({
        runtime: values.runtime,
        initial_prompt: values.initial_prompt,
        workspace_path: values.workspace_path || null,
      });
      onSuccess();
    } catch (err) {
      setSubmitError(formatApiError(err, "Could not create session."));
    }
  });

  return (
    <div
      ref={dialogRef}
      role="dialog"
      aria-modal="true"
      aria-labelledby="create-session-title"
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
      onClick={(e) => {
        if (e.target === e.currentTarget) {
          onClose();
        }
      }}
    >
      <div className="flex w-full max-w-lg flex-col gap-4 rounded-lg border border-border bg-background p-6 shadow-xl">
        <div className="flex items-start justify-between">
          <h2
            id="create-session-title"
            className="text-lg font-semibold tracking-tight"
          >
            New Agent Session
          </h2>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close"
            className="rounded-md p-1 text-muted-foreground hover:bg-accent hover:text-accent-foreground"
          >
            ✕
          </button>
        </div>

        <form onSubmit={onSubmit} className="flex flex-col gap-4">
          {/* Runtime selector */}
          <div className="flex flex-col gap-1.5">
            <label
              htmlFor="session-runtime"
              className="text-sm font-medium text-foreground"
            >
              Runtime
            </label>
            <select
              id="session-runtime"
              {...register("runtime")}
              aria-invalid={errors.runtime ? "true" : "false"}
              className="rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-primary"
            >
              {RUNTIME_OPTIONS.map((opt) => (
                <option key={opt} value={opt}>
                  {opt.replace("_", " ")}
                </option>
              ))}
            </select>
            {errors.runtime && (
              <p id="session-runtime-error" role="alert" className="text-xs text-destructive">
                {errors.runtime.message}
              </p>
            )}
          </div>

          {/* Initial prompt */}
          <div className="flex flex-col gap-1.5">
            <label
              htmlFor="session-prompt"
              className="text-sm font-medium text-foreground"
            >
              Prompt
            </label>
            <textarea
              id="session-prompt"
              rows={4}
              placeholder="Describe what you want the agent to do…"
              {...register("initial_prompt")}
              aria-invalid={errors.initial_prompt ? "true" : "false"}
              aria-describedby={
                errors.initial_prompt ? "session-prompt-error" : undefined
              }
              className="resize-none rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-primary"
            />
            {errors.initial_prompt && (
              <p id="session-prompt-error" role="alert" className="text-xs text-destructive">
                {errors.initial_prompt.message}
              </p>
            )}
          </div>

          {/* Workspace path (optional) */}
          <div className="flex flex-col gap-1.5">
            <label
              htmlFor="session-workspace"
              className="text-sm font-medium text-foreground"
            >
              Workspace path{" "}
              <span className="font-normal text-muted-foreground">(optional)</span>
            </label>
            <input
              id="session-workspace"
              type="text"
              placeholder="tenant_id/session_id"
              {...register("workspace_path")}
              aria-invalid={errors.workspace_path ? "true" : "false"}
              className="rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-primary"
            />
            {errors.workspace_path && (
              <p id="session-workspace-error" role="alert" className="text-xs text-destructive">
                {errors.workspace_path.message}
              </p>
            )}
          </div>

          {/* Submit error */}
          {submitError && (
            <p role="alert" className="text-sm text-destructive">
              {submitError}
            </p>
          )}

          {/* Actions */}
          <div className="flex items-center justify-end gap-3">
            <button
              type="button"
              onClick={onClose}
              className="rounded-md px-3 py-2 text-sm font-medium text-muted-foreground hover:bg-accent hover:text-accent-foreground"
            >
              Cancel
            </button>
            <button
              type="submit"
              disabled={isSubmitting}
              aria-busy={isSubmitting ? "true" : "false"}
              className="rounded-md bg-primary px-3 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-60"
            >
              {isSubmitting ? "Creating…" : "Create session"}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
