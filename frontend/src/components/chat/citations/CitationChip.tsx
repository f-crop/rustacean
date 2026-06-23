import { cn } from "@/lib/utils";
import type { CitationV1, SourceKind } from "@/types/citations";
import { buildGitHubUrl } from "./citation-utils";

const SUPPORTED_VERSION = "v1";

interface BadgeStyle {
  readonly label: string;
  readonly className: string;
}

const SOURCE_KIND_STYLES: Record<SourceKind, BadgeStyle> = {
  dense:  { label: "D", className: "bg-blue-100 text-blue-800 dark:bg-blue-900 dark:text-blue-200" },
  sparse: { label: "S", className: "bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200" },
  hybrid: { label: "H", className: "bg-purple-100 text-purple-800 dark:bg-purple-900 dark:text-purple-200" },
  rerank: { label: "R", className: "bg-yellow-100 text-yellow-800 dark:bg-yellow-900 dark:text-yellow-200" },
};

const FALLBACK_BADGE: BadgeStyle = {
  label: "?",
  className: "bg-muted text-muted-foreground",
};

interface CitationChipProps {
  readonly citation: CitationV1;
  /**
   * GitHub `owner/repo` slug (e.g. "f-crop/rustacean").
   * When provided the chip renders as an anchor that opens the canonical
   * GitHub blob URL in a new tab. When absent the chip is a non-interactive span.
   */
  readonly repoFullName?: string;
  readonly className?: string;
}

export function CitationChip({ citation, repoFullName, className }: CitationChipProps): JSX.Element {
  if (citation.version !== SUPPORTED_VERSION) {
    return (
      <span
        role="note"
        aria-label={`Unsupported citation version: ${citation.version}`}
        className={cn(
          "inline-flex items-center gap-1 rounded border border-yellow-300 bg-yellow-50 px-2 py-0.5 text-xs text-yellow-700",
          "dark:border-yellow-700 dark:bg-yellow-950 dark:text-yellow-300",
          className,
        )}
      >
        ⚠ citation {citation.version}
      </span>
    );
  }

  const { file_path, line_range, score, source_kind, commit_sha } = citation;
  const badge = SOURCE_KIND_STYLES[source_kind as SourceKind] ?? FALLBACK_BADGE;
  const shortSha = commit_sha.slice(0, 7);
  const ariaLabel = `${file_path} lines ${line_range.start} to ${line_range.end}, source kind ${source_kind}, score ${score.toFixed(2)}`;
  const tooltipTitle = `${shortSha} — ${source_kind}`;

  const chipClassName = cn(
    "inline-flex max-w-[20rem] items-center gap-1.5 rounded border border-border bg-muted/40 px-2 py-0.5",
    "transition-colors hover:bg-muted",
    "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1",
    className,
  );

  const inner = (
    <>
      <span
        aria-hidden="true"
        className={cn(
          "inline-flex h-4 w-4 shrink-0 items-center justify-center rounded text-[9px] font-bold",
          badge.className,
        )}
      >
        {badge.label}
      </span>
      <span className="min-w-0 truncate font-mono text-xs text-foreground">
        {file_path}:{line_range.start}–{line_range.end}
      </span>
      <span className="shrink-0 font-mono text-[10px] text-muted-foreground">
        {score.toFixed(2)}
      </span>
    </>
  );

  if (repoFullName) {
    return (
      <a
        href={buildGitHubUrl(repoFullName, citation)}
        target="_blank"
        rel="noopener noreferrer"
        aria-label={ariaLabel}
        title={tooltipTitle}
        className={chipClassName}
        data-testid="citation-chip"
        data-source-kind={source_kind}
      >
        {inner}
      </a>
    );
  }

  return (
    <span
      aria-label={ariaLabel}
      title={tooltipTitle}
      className={chipClassName}
      data-testid="citation-chip"
      data-source-kind={source_kind}
    >
      {inner}
    </span>
  );
}
