import { useState } from "react";
import { ChevronDown, ChevronUp, Copy, Check } from "lucide-react";
import PrismLight from "react-syntax-highlighter/dist/esm/prism-light";
import { oneDark } from "react-syntax-highlighter/dist/esm/styles/prism";
import json from "react-syntax-highlighter/dist/esm/languages/prism/json";
import { useCallback, useEffect, useRef } from "react";
import { MarkdownContent } from "../MarkdownContent";
import { deepDecodeJsonStrings, looksLikeMarkdown, needsTruncation } from "../tool-call-utils";

PrismLight.registerLanguage("json", json);

const TRUNCATE_LINES = 200;

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
      () => { /* clipboard write rejected */ },
    );
  }, [text]);

  return (
    <button
      type="button"
      onClick={handleCopy}
      className="rounded p-1 text-zinc-400 transition-colors hover:bg-zinc-700 hover:text-zinc-100"
      aria-label={copied ? "Copied" : "Copy to clipboard"}
    >
      {copied ? <Check className="h-3.5 w-3.5" /> : <Copy className="h-3.5 w-3.5" />}
    </button>
  );
}

function JsonBlock({ value, copyText }: { readonly value: string; readonly copyText?: string }): JSX.Element {
  const truncate = needsTruncation(value);
  const [showMore, setShowMore] = useState(false);
  const lines = value.split("\n");
  const displayValue = truncate && !showMore ? lines.slice(0, TRUNCATE_LINES).join("\n") : value;

  return (
    <div className="overflow-hidden rounded bg-zinc-900">
      <div className="flex items-center justify-end bg-zinc-800 px-2 py-1">
        <CopyButton text={copyText ?? value} />
      </div>
      <PrismLight
        language="json"
        style={oneDark}
        PreTag="div"
        customStyle={{ margin: 0, borderRadius: 0, fontSize: "0.75rem", lineHeight: "1.5" }}
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
            <><ChevronUp className="h-3.5 w-3.5" />Show less</>
          ) : (
            <><ChevronDown className="h-3.5 w-3.5" />Show more ({lines.length - TRUNCATE_LINES} more lines)</>
          )}
        </button>
      )}
    </div>
  );
}

interface JsonResultRendererProps {
  readonly result: unknown;
}

export function JsonResultRenderer({ result }: JsonResultRendererProps): JSX.Element {
  const rawText = typeof result === "string" ? result : JSON.stringify(result, null, 2);
  const isMarkdown = typeof result === "string" && looksLikeMarkdown(result);
  const truncate = needsTruncation(rawText);
  const [showMore, setShowMore] = useState(false);
  const lines = rawText.split("\n");
  const displayText = truncate && !showMore ? lines.slice(0, TRUNCATE_LINES).join("\n") : rawText;

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
              <><ChevronUp className="h-3.5 w-3.5" />Show less</>
            ) : (
              <><ChevronDown className="h-3.5 w-3.5" />Show more</>
            )}
          </button>
        )}
      </div>
    );
  }

  const decodedText = JSON.stringify(deepDecodeJsonStrings(result), null, 2) ?? rawText;
  return <JsonBlock value={decodedText} copyText={rawText} />;
}
