import { useState, useCallback, useRef, useEffect } from "react";

const STORAGE_KEY = "code-workspace.panel-widths.v1";
const MIN_PANEL = 160;
const MIN_CENTER = 320;
const DEFAULT_LEFT = 224; // equivalent to w-56
const DEFAULT_RIGHT = 256; // equivalent to w-64

interface PanelWidths {
  left: number;
  right: number;
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, v));
}

function loadWidths(): PanelWidths {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Record<string, unknown>;
      if (typeof parsed.left === "number" && typeof parsed.right === "number") {
        return { left: parsed.left, right: parsed.right };
      }
    }
  } catch {
    // ignore parse errors
  }
  return { left: DEFAULT_LEFT, right: DEFAULT_RIGHT };
}

function saveWidths(w: PanelWidths): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(w));
  } catch {
    // ignore quota errors
  }
}

type Side = "left" | "right";

interface DragState {
  side: Side;
  startX: number;
  startWidth: number;
}

export interface PanelResizeHandlers {
  leftWidth: number;
  rightWidth: number;
  startLeftDrag: (e: React.MouseEvent) => void;
  startRightDrag: (e: React.MouseEvent) => void;
  handleLeftKey: (e: React.KeyboardEvent) => void;
  handleRightKey: (e: React.KeyboardEvent) => void;
}

export function usePanelResize(): PanelResizeHandlers {
  const [widths, setWidths] = useState<PanelWidths>(loadWidths);
  const dragRef = useRef<DragState | null>(null);

  // Stable helpers that read window.innerWidth at call time (full-width page layout)
  const maxLeft = useCallback((w: PanelWidths) =>
    Math.max(MIN_PANEL, window.innerWidth - w.right - MIN_CENTER), []);

  const maxRight = useCallback((w: PanelWidths) =>
    Math.max(MIN_PANEL, window.innerWidth - w.left - MIN_CENTER), []);

  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      const d = dragRef.current;
      if (!d) return;
      const delta = e.clientX - d.startX;
      setWidths((prev) => {
        if (d.side === "left") {
          return { ...prev, left: clamp(d.startWidth + delta, MIN_PANEL, maxLeft(prev)) };
        }
        return { ...prev, right: clamp(d.startWidth - delta, MIN_PANEL, maxRight(prev)) };
      });
    };

    const onUp = () => {
      if (!dragRef.current) return;
      dragRef.current = null;
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
      setWidths((w) => { saveWidths(w); return w; });
    };

    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
    return () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
    };
  }, [maxLeft, maxRight]);

  const startLeftDrag = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    dragRef.current = { side: "left", startX: e.clientX, startWidth: widths.left };
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
  }, [widths.left]);

  const startRightDrag = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    dragRef.current = { side: "right", startX: e.clientX, startWidth: widths.right };
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
  }, [widths.right]);

  // Left handle: ArrowRight grows panel, ArrowLeft shrinks panel
  const handleLeftKey = useCallback((e: React.KeyboardEvent) => {
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    e.preventDefault();
    const step = e.shiftKey ? 50 : 10;
    const delta = e.key === "ArrowRight" ? step : -step;
    setWidths((prev) => {
      const next = { ...prev, left: clamp(prev.left + delta, MIN_PANEL, maxLeft(prev)) };
      saveWidths(next);
      return next;
    });
  }, [maxLeft]);

  // Right handle: ArrowLeft grows panel, ArrowRight shrinks panel
  const handleRightKey = useCallback((e: React.KeyboardEvent) => {
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    e.preventDefault();
    const step = e.shiftKey ? 50 : 10;
    const delta = e.key === "ArrowLeft" ? step : -step;
    setWidths((prev) => {
      const next = { ...prev, right: clamp(prev.right + delta, MIN_PANEL, maxRight(prev)) };
      saveWidths(next);
      return next;
    });
  }, [maxRight]);

  return { leftWidth: widths.left, rightWidth: widths.right, startLeftDrag, startRightDrag, handleLeftKey, handleRightKey };
}
