import { useState } from "react";
import { Loader2, CheckCircle2, XCircle, ChevronDown, ChevronUp } from "lucide-react";
import { cn } from "@/lib/utils";
import { getToolRenderer, getArgPreview, deepDecodeJsonStrings } from "./tool-call-utils";
import { JsonResultRenderer } from "./tool-renderers/JsonResultRenderer";
import { BashResultRenderer } from "./tool-renderers/BashResultRenderer";
import { ReadResultRenderer } from "./tool-renderers/ReadResultRenderer";

function InputBlock({ input }: { readonly input: unknown }): JSX.Element {
  const text = JSON.stringify(deepDecodeJsonStrings(input), null, 2);
  return (
    <div className="overflow-hidden rounded bg-zinc-900">
      <pre className="overflow-x-auto px-3 py-2 font-mono text-xs leading-relaxed text-zinc-300 whitespace-pre">
        {text}
      </pre>
    </div>
  );
}

function SectionLabel({ children }: { readonly children: string }): JSX.Element {
  return (
    <p className="mb-1 text-[10px] font-semibold uppercase tracking-widest text-zinc-500">
      {children}
    </p>
  );
}

function isEmptyInput(input: unknown): boolean {
  if (input === null || input === undefined) return true;
  if (
    typeof input === "object" &&
    !Array.isArray(input) &&
    Object.keys(input as Record<string, unknown>).length === 0
  )
    return true;
  return false;
}

interface ToolCallBlockProps {
  readonly name: string;
  readonly input: unknown;
  readonly result: unknown | null;
  readonly isError: boolean;
  readonly timestamp?: number | undefined;
}

export function ToolCallBlock({
  name,
  input,
  result,
  isError,
  timestamp,
}: ToolCallBlockProps): JSX.Element {
  const [open, setOpen] = useState(false);
  const hasResult = result !== null;
  const running = !hasResult;
  const error = hasResult && isError;

  const statusLabel = running ? "Running…" : error ? "Error" : "Done";
  const argsPreview = getArgPreview(name, input);
  const rendererType = getToolRenderer(name);

  return (
    <div
      className={cn(
        "overflow-hidden rounded-lg border bg-zinc-900/60",
        error ? "border-destructive/40" : "border-zinc-800",
      )}
      data-testid="tool-call-block"
    >
      {timestamp !== undefined && (
        <time
          className="block px-3 pt-2 text-[10px] text-zinc-600"
          dateTime={new Date(timestamp).toISOString()}
        >
          {new Date(timestamp).toLocaleString("en-US", {
            month: "numeric",
            day: "numeric",
            year: "numeric",
            hour: "numeric",
            minute: "2-digit",
            second: "2-digit",
            hour12: true,
          })}
        </time>
      )}
      <button
        type="button"
        className="flex w-full items-center gap-2 px-3 py-2 text-left hover:bg-zinc-800/50 transition-colors"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        aria-label={`${name} tool call — ${statusLabel}`}
      >
        <span
          className="shrink-0 select-none font-bold text-violet-400"
          aria-hidden="true"
        >
          *
        </span>
        <span className="shrink-0 font-semibold text-sm text-zinc-200">
          {name}
        </span>
        {argsPreview && (
          <span className="min-w-0 flex-1 truncate text-xs text-zinc-500">
            {argsPreview}
          </span>
        )}
        <span className="ml-auto flex shrink-0 items-center gap-1.5">
          {running ? (
            <Loader2 className="h-3.5 w-3.5 animate-spin text-blue-400" aria-hidden="true" />
          ) : error ? (
            <XCircle className="h-3.5 w-3.5 text-destructive" aria-hidden="true" />
          ) : (
            <CheckCircle2 className="h-3.5 w-3.5 text-green-500" aria-hidden="true" />
          )}
          <span className="sr-only">{statusLabel}</span>
          {open ? (
            <ChevronUp className="h-3.5 w-3.5 text-zinc-500" aria-hidden="true" />
          ) : (
            <ChevronDown className="h-3.5 w-3.5 text-zinc-500" aria-hidden="true" />
          )}
        </span>
      </button>

      <div
        className={cn(
          "grid transition-[grid-template-rows] duration-200 ease-out",
          open ? "grid-rows-[1fr]" : "grid-rows-[0fr]",
        )}
      >
        <div className="overflow-hidden">
          <div className="border-t border-zinc-800 px-3 py-3 space-y-3">
            {!isEmptyInput(input) && (
              <div>
                <SectionLabel>Input</SectionLabel>
                <InputBlock input={input} />
              </div>
            )}
            {hasResult && (
              <div>
                <SectionLabel>Result</SectionLabel>
                {rendererType === "read" ? (
                  <ReadResultRenderer result={result} input={input} />
                ) : rendererType === "bash" ? (
                  <BashResultRenderer result={result} />
                ) : (
                  <JsonResultRenderer result={result} />
                )}
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

