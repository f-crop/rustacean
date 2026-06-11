import { useState, useCallback, useEffect, useRef } from "react";
import {
  Loader2,
  CheckCircle2,
  XCircle,
  Copy,
  Check,
  ChevronDown,
  ChevronUp,
} from "lucide-react";
import PrismLight from "react-syntax-highlighter/dist/esm/prism-light";
import { oneDark } from "react-syntax-highlighter/dist/esm/styles/prism";
import json from "react-syntax-highlighter/dist/esm/languages/prism/json";
import { cn } from "@/lib/utils";
import { MarkdownContent } from "./MarkdownContent";
import { looksLikeMarkdown, needsTruncation } from "./tool-call-utils";

PrismLight.registerLanguage("json", json);

const TRUNCATE_LINES = 200;

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

function CopyButton({ text }: { readonly text: string }): JSX.Element {
  const [copied, setCopied] = useState(false);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    return () => {
      if (timerRef.current !== null) clearTimeout(timerRef.current);
    };
  }, []);

  const handleCopy = useCallback(() => {
    if (!navigator.clipboard) return;
    void navigator.clipboard.writeText(text).then(
      () => {
        setCopied(true);
        if (timerRef.current !== null) clearTimeout(timerRef.current);
        timerRef.current = setTimeout(() => setCopied(false), 2000);
      },
      () => {
        /* clipboard write rejected — silently fail */
      },
    );
  }, [text]);

  return (
    <button
      type="button"
      onClick={handleCopy}
      className="rounded p-1 text-zinc-400 transition-colors hover:bg-zinc-700 hover:text-zinc-100"
      aria-label={copied ? "Copied" : "Copy to clipboard"}
    >
      {copied ? (
        <Check className="h-3.5 w-3.5" />
      ) : (
        <Copy className="h-3.5 w-3.5" />
      )}
    </button>
  );
}

function JsonBlock({ value }: { readonly value: string }): JSX.Element {
  const truncate = needsTruncation(value);
  const [showMore, setShowMore] = useState(false);
  const lines = value.split("\n");
  const displayValue =
    truncate && !showMore ? lines.slice(0, TRUNCATE_LINES).join("\n") : value;

  return (
    <div className="overflow-hidden rounded bg-zinc-900">
      <div className="flex items-center justify-end bg-zinc-800 px-2 py-1">
        <CopyButton text={value} />
      </div>
      <PrismLight
        language="json"
        style={oneDark}
        PreTag="div"
        customStyle={{
          margin: 0,
          borderRadius: 0,
          fontSize: "0.75rem",
          lineHeight: "1.5",
        }}
      >
        {displayValue}
      </PrismLight>
      {truncate && (
        <button
          type="button"
          onClick={() => setShowMore((s) => !s)}
          className="flex w-full items-center justify-center gap-1 bg-zinc-800 py-1.5 text-xs text-zinc-400 hover:text-zinc-200"
        >
          {showMore ? (
            <>
              <ChevronUp className="h-3.5 w-3.5" />
              Show less
            </>
          ) : (
            <>
              <ChevronDown className="h-3.5 w-3.5" />
              Show more ({lines.length - TRUNCATE_LINES} more lines)
            </>
          )}
        </button>
      )}
    </div>
  );
}

function ResultBlock({ result }: { readonly result: unknown }): JSX.Element {
  const rawText =
    typeof result === "string"
      ? result
      : JSON.stringify(result, null, 2);
  const isMarkdown = typeof result === "string" && looksLikeMarkdown(result);
  const truncate = needsTruncation(rawText);
  const [showMore, setShowMore] = useState(false);
  const lines = rawText.split("\n");
  const displayText =
    truncate && !showMore ? lines.slice(0, TRUNCATE_LINES).join("\n") : rawText;

  if (isMarkdown) {
    return (
      <div className="relative rounded border border-border bg-background p-3 text-sm">
        <div className="absolute right-2 top-2">
          <CopyButton text={rawText} />
        </div>
        <MarkdownContent text={displayText} />
        {truncate && (
          <button
            type="button"
            onClick={() => setShowMore((s) => !s)}
            className="mt-2 flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
          >
            {showMore ? (
              <>
                <ChevronUp className="h-3.5 w-3.5" />
                Show less
              </>
            ) : (
              <>
                <ChevronDown className="h-3.5 w-3.5" />
                Show more
              </>
            )}
          </button>
        )}
      </div>
    );
  }

  return <JsonBlock value={rawText} />;
}

interface ToolCallBlockProps {
  readonly name: string;
  readonly input: unknown;
  readonly result: unknown | null;
  readonly isError: boolean;
  readonly sequence: number;
}

export function ToolCallBlock({
  name,
  input,
  result,
  isError,
  sequence,
}: ToolCallBlockProps): JSX.Element {
  const [open, setOpen] = useState(false);
  const hasResult = result !== null;
  const running = !hasResult;
  const error = hasResult && isError;

  const containerClass = error
    ? "rounded border border-destructive/30 bg-destructive/5 px-3 py-2"
    : hasResult
      ? "rounded border border-green-200 bg-green-50/60 px-3 py-2 dark:border-green-900/40 dark:bg-green-950/20"
      : "rounded border border-blue-200 bg-blue-50/60 px-3 py-2 dark:border-blue-900/40 dark:bg-blue-950/20";

  const statusLabel = running ? "Running…" : error ? "Error" : "Done";

  return (
    <div className={containerClass} data-testid="tool-call-block">
      <button
        type="button"
        className="flex w-full items-center gap-2 text-left"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        aria-label={`${name} tool call — ${statusLabel}`}
      >
        <span className="shrink-0 font-mono text-[10px] text-muted-foreground/50 tabular-nums">
          #{sequence}
        </span>
        <span className="max-w-[180px] truncate rounded bg-muted px-1.5 py-0.5 font-mono text-xs font-medium">
          {name}
        </span>
        {running ? (
          <Loader2
            className="h-4 w-4 animate-spin text-blue-500"
            aria-hidden="true"
          />
        ) : error ? (
          <XCircle
            className="h-4 w-4 text-destructive"
            aria-hidden="true"
          />
        ) : (
          <CheckCircle2
            className="h-4 w-4 text-green-600 dark:text-green-400"
            aria-hidden="true"
          />
        )}
        <span className="ml-auto shrink-0 text-xs text-muted-foreground" aria-hidden="true">
          {open ? "▲" : "▼"}
        </span>
      </button>
      <div
        className={cn(
          "grid transition-[grid-template-rows] duration-200 ease-out",
          open ? "grid-rows-[1fr]" : "grid-rows-[0fr]",
        )}
      >
        <div className="overflow-hidden">
          <div className="mt-2 space-y-2">
            {!isEmptyInput(input) && (
              <div>
                <p className="mb-1 text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
                  Input
                </p>
                <JsonBlock value={JSON.stringify(input, null, 2)} />
              </div>
            )}
            {hasResult && (
              <div>
                <p className="mb-1 text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
                  Result
                </p>
                <ResultBlock result={result} />
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
