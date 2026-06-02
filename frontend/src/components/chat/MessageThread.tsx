import { useEffect, useRef } from "react";
import { ToolCallBlock } from "./ToolCallBlock";
import type { TranscriptItem, AssistantItem } from "./transcript";

interface MessageThreadProps {
  readonly items: ReadonlyArray<TranscriptItem>;
  readonly isStreaming: boolean;
}

export function MessageThread({ items, isStreaming }: MessageThreadProps): JSX.Element {
  const bottomRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [items]);

  if (items.length === 0 && !isStreaming) {
    return (
      <div className="flex flex-1 items-center justify-center">
        <p className="text-sm text-muted-foreground">Send a message to start the conversation.</p>
      </div>
    );
  }

  return (
    <div className="flex flex-1 flex-col gap-4 overflow-y-auto px-4 py-4">
      {items.map((item) => {
        if (item.kind === "user") {
          return <UserBubble key={item.id} text={item.text} />;
        }
        if (item.kind === "assistant") {
          return <AssistantBubble key={item.id} items={item.items} />;
        }
        if (item.kind === "error") {
          return item.code !== undefined
            ? <ErrorBanner key={item.id} message={item.message} code={item.code} />
            : <ErrorBanner key={item.id} message={item.message} />;
        }
        return null;
      })}
      {isStreaming && (
        <div
          role="status"
          aria-live="polite"
          aria-label="Waiting for response"
          className="flex items-center gap-1.5 text-xs text-muted-foreground"
        >
          <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-muted-foreground/60" />
          <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-muted-foreground/60 [animation-delay:0.2s]" />
          <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-muted-foreground/60 [animation-delay:0.4s]" />
        </div>
      )}
      <div ref={bottomRef} />
    </div>
  );
}

function UserBubble({ text }: { readonly text: string }): JSX.Element {
  return (
    <div className="flex justify-end">
      <div className="max-w-[80%] rounded-2xl rounded-tr-sm bg-primary px-4 py-2.5 text-primary-foreground">
        <p className="whitespace-pre-wrap text-sm leading-relaxed">{text}</p>
      </div>
    </div>
  );
}

function AssistantBubble({ items }: { readonly items: ReadonlyArray<AssistantItem> }): JSX.Element {
  if (items.length === 0) return <></>;

  return (
    <div className="flex justify-start">
      <div className="max-w-[90%] space-y-2">
        {items.map((item, i) => {
          if (item.type === "text") {
            return (
              <p key={i} className="whitespace-pre-wrap text-sm leading-relaxed text-foreground">
                {item.text}
              </p>
            );
          }
          if (item.type === "thinking") {
            return <ThinkingBlock key={i} thinking={item.thinking} sequence={item.seq} />;
          }
          if (item.type === "tool_use") {
            const result = findToolResult(items, item.id);
            return (
              <ToolCallBlock
                key={item.id}
                name={item.name}
                input={item.input}
                result={result?.content ?? null}
                isError={result?.isError ?? false}
                sequence={item.seq}
              />
            );
          }
          if (item.type === "error") {
            return (
              <p key={i} className="text-sm text-destructive">
                {item.message}
              </p>
            );
          }
          return null;
        })}
      </div>
    </div>
  );
}

function findToolResult(
  items: ReadonlyArray<AssistantItem>,
  toolUseId: string,
): { content: unknown; isError: boolean } | null {
  for (const item of items) {
    if (item.type === "tool_result" && item.toolUseId === toolUseId) {
      return { content: item.content, isError: item.isError };
    }
  }
  return null;
}

function ThinkingBlock({
  thinking,
  sequence,
}: {
  readonly thinking: string;
  readonly sequence: number;
}): JSX.Element {
  const preview = thinking.slice(0, 80);
  return (
    <details className="rounded border border-border/40 bg-muted/10 px-3 py-2 text-xs">
      <summary className="cursor-pointer text-muted-foreground">
        #{sequence} Thinking: {preview}{thinking.length > 80 ? "…" : ""}
      </summary>
      <p className="mt-2 whitespace-pre-wrap text-muted-foreground">{thinking}</p>
    </details>
  );
}

function ErrorBanner({
  message,
  code,
}: {
  readonly message: string;
  readonly code?: string;
}): JSX.Element {
  return (
    <div
      role="alert"
      className="rounded border border-destructive/30 bg-destructive/5 px-4 py-3 text-center"
    >
      {code && <p className="text-xs font-medium uppercase text-muted-foreground">{code}</p>}
      <p className="text-sm text-destructive">{message}</p>
    </div>
  );
}
