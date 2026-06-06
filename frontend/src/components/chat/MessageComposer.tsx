import { useRef, type FormEvent, type KeyboardEvent } from "react";

interface MessageComposerProps {
  readonly value: string;
  readonly onChange: (value: string) => void;
  readonly onSend: (content: string) => void;
  /** Fully disables the composer — used when no session exists yet (createSession pending). */
  readonly isDisabled: boolean;
  /**
   * True while the assistant is streaming or a message POST is in flight.
   * The textarea stays enabled so the user can type; submitting queues the message
   * instead of sending immediately.
   */
  readonly isQueuing?: boolean;
  /** Messages waiting to be sent after the current assistant turn completes. */
  readonly queuedMessages?: ReadonlyArray<string>;
}

export function MessageComposer({
  value,
  onChange,
  onSend,
  isDisabled,
  isQueuing = false,
  queuedMessages = [],
}: MessageComposerProps): JSX.Element {
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const handleSubmit = (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    const trimmed = value.trim();
    if (!trimmed || isDisabled) return;
    onSend(trimmed);
    onChange("");
    textareaRef.current?.focus();
  };

  const handleKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      const trimmed = value.trim();
      if (!trimmed || isDisabled) return;
      onSend(trimmed);
      onChange("");
    }
  };

  return (
    <div className="border-t border-border bg-background">
      {queuedMessages.length > 0 && (
        <div
          className="flex flex-col gap-1 px-3 pt-2 pb-1"
          data-testid="queued-messages"
        >
          {queuedMessages.map((msg, i) => (
            <div
              key={i} /* index-key is fine: queue items are display-only, never reordered */
              className="inline-flex items-center gap-1.5 self-end rounded-md bg-muted px-2.5 py-1 text-xs text-muted-foreground"
              data-testid="queued-message-chip"
            >
              <span className="max-w-[16rem] truncate">{msg}</span>
              <span className="whitespace-nowrap opacity-60">· Will send when current reply finishes</span>
            </div>
          ))}
        </div>
      )}
      <form
        onSubmit={handleSubmit}
        className="flex gap-2 p-3"
      >
        <textarea
          ref={textareaRef}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder={
            isQueuing
              ? "Type to queue — will send after current reply…"
              : "Type a message… (Enter to send, Shift+Enter for newline)"
          }
          disabled={isDisabled}
          rows={2}
          aria-label="Chat message"
          className="flex-1 resize-none rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring disabled:cursor-not-allowed disabled:opacity-50"
        />
        <button
          type="submit"
          disabled={isDisabled || value.trim().length === 0}
          className="self-end rounded-md bg-primary px-3 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-50"
        >
          {isQueuing ? "Queue" : "Send"}
        </button>
      </form>
    </div>
  );
}
