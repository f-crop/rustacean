import { useEffect, useRef } from "react";
import { toast } from "sonner";
import { useForm } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";
import {
  createSessionFormSchema,
  RUNTIME_OPTIONS,
  type CreateSessionFormValues,
} from "@/lib/validation/agentSessions";
import { formatApiError } from "@/lib/errors/api";

const FOCUSABLE_SELECTOR =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

function getFocusableElements(container: HTMLElement | null): HTMLElement[] {
  if (!container) return [];
  return Array.from(container.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR));
}

interface CreateSessionDialogProps {
  readonly isPending: boolean;
  readonly onSubmit: (values: CreateSessionFormValues) => Promise<void>;
  readonly onClose: () => void;
}

export function CreateSessionDialog({
  isPending,
  onSubmit,
  onClose,
}: CreateSessionDialogProps): JSX.Element {
  const dialogRef = useRef<HTMLDivElement>(null);
  const previousFocusRef = useRef<HTMLElement | null>(null);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

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
        onCloseRef.current();
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
  }, []);

  const {
    handleSubmit,
    register,
    formState: { errors },
  } = useForm<CreateSessionFormValues>({
    resolver: zodResolver(createSessionFormSchema),
    defaultValues: {
      runtime: "claude_code",
      initial_prompt: "",
      workspace_path: "",
    },
  });

  const handleFormSubmit = handleSubmit(async (values) => {
    try {
      await onSubmit(values);
    } catch (err) {
      toast.error(formatApiError(err, "Could not create session."));
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
            New agent session
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

        <form onSubmit={handleFormSubmit} className="flex flex-col gap-4">
          <div className="flex flex-col gap-1.5">
            <label htmlFor="session-runtime" className="text-sm font-medium">
              Runtime <span className="text-destructive">*</span>
            </label>
            <select
              id="session-runtime"
              {...register("runtime")}
              className="rounded-md border border-border bg-background px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              aria-describedby={errors.runtime ? "runtime-error" : undefined}
              aria-invalid={errors.runtime ? "true" : undefined}
            >
              {RUNTIME_OPTIONS.map((opt) => (
                <option key={opt.value} value={opt.value}>
                  {opt.label}
                </option>
              ))}
            </select>
            {errors.runtime ? (
              <p id="runtime-error" className="text-xs text-destructive" role="alert">
                {errors.runtime.message}
              </p>
            ) : null}
          </div>

          <div className="flex flex-col gap-1.5">
            <label htmlFor="session-prompt" className="text-sm font-medium">
              Initial prompt
            </label>
            <textarea
              id="session-prompt"
              {...register("initial_prompt")}
              rows={3}
              placeholder="Optional prompt to start the agent session…"
              className="rounded-md border border-border bg-background px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              aria-describedby={errors.initial_prompt ? "prompt-error" : undefined}
              aria-invalid={errors.initial_prompt ? "true" : undefined}
            />
            {errors.initial_prompt ? (
              <p id="prompt-error" className="text-xs text-destructive" role="alert">
                {errors.initial_prompt.message}
              </p>
            ) : null}
          </div>

          <div className="flex flex-col gap-1.5">
            <label htmlFor="session-workspace" className="text-sm font-medium">
              Workspace path
            </label>
            <input
              id="session-workspace"
              type="text"
              {...register("workspace_path")}
              placeholder="Defaults to tenant_id/session_id"
              className="rounded-md border border-border bg-background px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              aria-describedby={errors.workspace_path ? "workspace-error" : undefined}
              aria-invalid={errors.workspace_path ? "true" : undefined}
            />
            {errors.workspace_path ? (
              <p id="workspace-error" className="text-xs text-destructive" role="alert">
                {errors.workspace_path.message}
              </p>
            ) : null}
          </div>

          <div className="flex justify-end gap-2">
            <button
              type="button"
              onClick={onClose}
              className="rounded-md border border-border px-3 py-2 text-sm font-medium text-foreground hover:bg-accent hover:text-accent-foreground"
            >
              Cancel
            </button>
            <button
              type="submit"
              disabled={isPending}
              className="rounded-md bg-primary px-3 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-60"
            >
              {isPending ? "Creating…" : "Create session"}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
