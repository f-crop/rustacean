import { useState } from "react";
import { useSearch, fqnToB64, type SearchResult } from "@/api/hooks/useCodeIntel";
import { formatApiError } from "@/lib/errors/api";
import { cn } from "@/lib/utils";

interface SearchPanelProps {
  readonly onSelect: (fqn: string, fqnB64: string) => void;
}

export function SearchPanel({ onSelect }: SearchPanelProps): JSX.Element {
  const [query, setQuery] = useState("");
  const search = useSearch();

  const handleSubmit = (e: React.FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    const trimmed = query.trim();
    if (trimmed.length > 0) {
      search.mutate({ q: trimmed });
    }
  };

  return (
    <div className="flex h-full flex-col">
      <form onSubmit={handleSubmit} className="shrink-0 border-b border-border p-2">
        <div className="flex gap-1.5">
          <input
            aria-label="Search query"
            data-testid="search-input"
            type="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search symbols…"
            className={cn(
              "min-w-0 flex-1 rounded border border-border bg-background px-2 py-1",
              "font-mono text-xs text-foreground placeholder:text-muted-foreground",
              "focus:outline-none focus:ring-1 focus:ring-ring",
            )}
          />
          <button
            type="submit"
            disabled={search.isPending || query.trim().length === 0}
            className={cn(
              "shrink-0 rounded bg-primary px-2 py-1 text-xs font-medium text-primary-foreground",
              "disabled:opacity-50",
            )}
          >
            {search.isPending ? "…" : "Go"}
          </button>
        </div>
      </form>

      <div className="flex-1 overflow-y-auto">
        {search.isPending && (
          <p className="p-3 text-xs text-muted-foreground">Searching…</p>
        )}

        {search.isError && (
          <p className="p-3 text-xs text-destructive" role="alert">
            {formatApiError(search.error, "Search failed.")}
          </p>
        )}

        {search.data && search.data.results.length === 0 && (
          <p className="p-3 text-xs text-muted-foreground">No results found.</p>
        )}

        {search.data && search.data.results.length > 0 && (
          <ul
            aria-label="Search results"
            data-testid="search-results"
            className="py-1"
          >
            {search.data.results.map((result) => (
              <SearchResultItem
                key={result.fqn}
                result={result}
                onSelect={onSelect}
              />
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

interface SearchResultItemProps {
  readonly result: SearchResult;
  readonly onSelect: (fqn: string, fqnB64: string) => void;
}

function SearchResultItem({ result, onSelect }: SearchResultItemProps): JSX.Element {
  const handleClick = () => {
    onSelect(result.fqn, fqnToB64(result.fqn));
  };

  return (
    <li>
      <button
        type="button"
        onClick={handleClick}
        aria-label={`Open ${result.fqn}`}
        className={cn(
          "w-full px-3 py-2 text-left",
          "hover:bg-accent hover:text-accent-foreground",
          "focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring",
        )}
      >
        <p className="truncate font-mono text-xs font-medium text-foreground">
          {result.fqn}
        </p>
        <p className="mt-0.5 text-[10px] text-muted-foreground">
          {result.crate_name}
          <span className="ml-2 opacity-60">{(result.score * 100).toFixed(0)}%</span>
        </p>
      </button>
    </li>
  );
}
