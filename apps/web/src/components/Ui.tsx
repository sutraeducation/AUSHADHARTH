import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";

/**
 * A document's state, said in words.
 *
 * The tone only tints what the text already says: a reader who cannot distinguish the tints — or
 * who is reading a printed page — still reads "Posted", "Draft" or "Read-only". Colour is never
 * the carrier of the meaning, which is why the label is a child and not an optional prop.
 */
export function StatusBadge({
  tone = "neutral",
  children,
}: {
  tone?: "draft" | "posted" | "neutral" | "void";
  children: ReactNode;
}) {
  return <span className={`status-badge status-badge--${tone}`}>{children}</span>;
}

/**
 * A table that may be wider than the screen it is read on.
 *
 * A statutory particular is not something to drop on a narrow screen, and folding a register into
 * cards would lose the columns a reader is checking against a printed book. So the table keeps
 * every column and scrolls sideways — and says so, because a scrollable area that looks like a
 * clipped one is how a reader concludes the data is missing. The note appears only when the
 * content really does overflow, so it never nags on a wide screen.
 */
export function ScrollableTable({
  children,
  hint = "Scroll sideways for the rest of this table",
}: {
  children: ReactNode;
  hint?: string;
}) {
  const scroller = useRef<HTMLDivElement>(null);
  const [overflowing, setOverflowing] = useState(false);
  const [atEnd, setAtEnd] = useState(false);

  const measure = useCallback(() => {
    const node = scroller.current;
    if (!node) return;
    const overflows = node.scrollWidth - node.clientWidth > 1;
    setOverflowing(overflows);
    setAtEnd(!overflows || node.scrollLeft + node.clientWidth >= node.scrollWidth - 1);
  }, []);

  useEffect(() => {
    const node = scroller.current;
    if (!node) return;
    measure();
    // ResizeObserver is missing in some test environments; the measurement above still runs, so the
    // table is never worse off than a plain scroller.
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(node);
    for (const child of Array.from(node.children)) observer.observe(child);
    return () => observer.disconnect();
  }, [measure]);

  return (
    <div className="table-scroll-shell">
      <div
        ref={scroller}
        className="table-scroll"
        data-overflowing={overflowing ? "true" : "false"}
        data-at-end={atEnd ? "true" : "false"}
        onScroll={measure}
      >
        {children}
      </div>
      {overflowing && (
        <p className="table-scroll__hint" data-testid="table-scroll-hint">
          {hint} <span aria-hidden="true">→</span>
        </p>
      )}
    </div>
  );
}
