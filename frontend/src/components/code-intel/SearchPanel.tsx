export function SearchPanel(): JSX.Element {
  return (
    <div
      className="flex h-full flex-col items-center justify-center gap-2 px-4 text-center"
      aria-label="Search panel"
    >
      <p className="text-sm font-medium text-muted-foreground">Search coming soon</p>
      <p className="text-xs text-muted-foreground">
        Semantic search requires{" "}
        <span className="font-mono">POST /v1/search</span> (REQ-DP-01 — pending).
      </p>
    </div>
  );
}
