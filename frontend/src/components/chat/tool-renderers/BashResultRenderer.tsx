import { useState } from "react";
import { ChevronDown, ChevronUp } from "lucide-react";
import { needsTruncation } from "../tool-call-utils";

const TRUNCATE_LINES = 200;

interface BashResultRendererProps {
  readonly result: unknown;
}

export function BashResultRenderer({ result }: BashResultRendererProps): JSX.Element {
  const text = typeof result === "string" ? result : JSON.stringify(result, null, 2);
  const truncate = needsTruncation(text);
  const [showMore, setShowMore] = useState(false);
  const lines = text.split("\n");
  const displayText = truncate && !showMore ? lines.slice(0, TRUNCATE_LINES).join("\n") : text;

  return (
    <div className="overflow-hidden rounded bg-muted">
      <pre className="overflow-x-auto px-3 py-2 font-mono text-xs leading-relaxed text-foreground whitespace-pre">
        {displayText}
      </pre>
      {truncate && (
        <button
          type="button"
          onClick={() => setShowMore((s) => !s)}
          className="flex w-full items-center justify-center gap-1 bg-accent py-1.5 text-xs text-muted-foreground hover:text-foreground"
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
