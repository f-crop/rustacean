export function RelationsPanel(): JSX.Element {
  return (
    <div
      className="flex h-full flex-col items-center justify-center gap-2 px-4 text-center"
      aria-label="Relations panel"
    >
      <p className="text-sm font-medium text-muted-foreground">Callers / callees coming soon</p>
      <p className="text-xs text-muted-foreground">
        Graph traversal requires REQ-DP-03 (caller/callee endpoint — pending).
      </p>
    </div>
  );
}
