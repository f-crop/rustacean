import { useMe } from "@/api/hooks/useMe";
import { useRepos } from "@/api";
import { cn } from "@/lib/utils";
import {
  parseCitationResult,
  buildGitHubUrl,
  sourceKindBadgeClass,
  type CitationV1,
  type SourceKind,
} from "../citation-utils";

interface CitationChipsRendererProps {
  readonly result: unknown;
}

export function CitationChipsRenderer({ result }: CitationChipsRendererProps): JSX.Element {
  const me = useMe();
  const repos = useRepos(me.data?.current_tenant.id ?? "");

  const items = parseCitationResult(result);

  if (items.length === 0) {
    return (
      <p className="text-xs italic text-muted-foreground" data-testid="citation-empty">
        No citations available.
      </p>
    );
  }

  const repoMap = new Map<string, string>(
    (repos.data?.repos ?? []).map((r) => [r.repo_id, r.full_name]),
  );

  // Group valid citations by repo_id; preserve insertion order
  const groups = new Map<string, CitationV1[]>();
  const unknownVersions: string[] = [];

  for (const item of items) {
    if (item.type === "unknown_version") {
      unknownVersions.push(item.version);
      continue;
    }
    const { repo_id } = item.citation;
    const bucket = groups.get(repo_id) ?? [];
    bucket.push(item.citation);
    groups.set(repo_id, bucket);
  }

  const multiRepo = groups.size > 1;

  return (
    <div className="space-y-2" data-testid="citation-chips-renderer">
      {[...groups.entries()].map(([repoId, cits]) => {
        const fullName = repoMap.get(repoId);
        return (
          <div key={repoId}>
            {multiRepo && fullName !== undefined && (
              <p className="mb-1 text-[10px] font-semibold uppercase tracking-widest text-muted-foreground">
                {fullName}
              </p>
            )}
            <div className="flex flex-wrap gap-1.5">
              {cits.map((c, i) => (
                <CitationChip key={i} citation={c} fullName={fullName} />
              ))}
            </div>
          </div>
        );
      })}
      {unknownVersions.length > 0 && (
        <div
          className="rounded-md border border-amber-300 bg-amber-50 px-3 py-2 text-xs text-amber-700 dark:border-amber-700/40 dark:bg-amber-900/20 dark:text-amber-300"
          role="alert"
          data-testid="citation-version-warning"
        >
          {unknownVersions.length === 1
            ? `1 citation used an unknown format (version "${unknownVersions[0]}") and cannot be displayed.`
            : `${unknownVersions.length} citations used an unknown format and cannot be displayed.`}
        </div>
      )}
    </div>
  );
}

interface CitationChipProps {
  readonly citation: CitationV1;
  readonly fullName: string | undefined;
}

function CitationChip({ citation, fullName }: CitationChipProps): JSX.Element {
  const { file_path, line_range, score, source_kind } = citation;
  const url = fullName !== undefined ? buildGitHubUrl(citation, fullName) : null;
  const rangeLabel = `${line_range.start}-${line_range.end}`;
  const chipLabel = `${file_path}:${rangeLabel}`;
  const ariaLabel = `${chipLabel} — ${source_kind} (score ${score.toFixed(2)})`;

  const chipClass =
    "inline-flex max-w-xs items-center gap-1.5 rounded-md border border-border bg-muted px-2 py-1 text-xs transition-colors hover:bg-accent";

  const inner = (
    <>
      <span
        className="max-w-[160px] truncate font-mono text-foreground/90"
        title={chipLabel}
      >
        {chipLabel}
      </span>
      <SourceKindBadge kind={source_kind} />
      <span className="shrink-0 tabular-nums text-muted-foreground">
        {score.toFixed(2)}
      </span>
    </>
  );

  if (url !== null) {
    return (
      <a
        href={url}
        target="_blank"
        rel="noopener noreferrer"
        className={chipClass}
        aria-label={ariaLabel}
        data-testid="citation-chip"
      >
        {inner}
      </a>
    );
  }

  return (
    <span className={chipClass} aria-label={ariaLabel} data-testid="citation-chip">
      {inner}
    </span>
  );
}

function SourceKindBadge({ kind }: { readonly kind: SourceKind }): JSX.Element {
  return (
    <span
      className={cn(
        "shrink-0 rounded px-1 py-0.5 text-[10px] font-medium",
        sourceKindBadgeClass(kind),
      )}
      data-testid={`source-kind-badge-${kind}`}
    >
      {kind}
    </span>
  );
}
