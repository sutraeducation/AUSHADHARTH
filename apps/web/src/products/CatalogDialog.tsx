import { useEffect, useId, useRef, type ReactNode } from "react";
import { createPortal } from "react-dom";

export function CatalogDialog({ title, description, onClose, children, wide = false }: {
  title: string;
  description?: string;
  onClose: () => void;
  children: ReactNode;
  wide?: boolean;
}) {
  const panel = useRef<HTMLDivElement>(null);
  const previousFocus = useRef(document.activeElement as HTMLElement | null);
  const titleId = useId();
  const closeRef = useRef(onClose);
  useEffect(() => { closeRef.current = onClose; }, [onClose]);
  useEffect(() => {
    const element = panel.current;
    const focusable = () => Array.from(element?.querySelectorAll<HTMLElement>(
      'button:not([disabled]), input:not([disabled]), select:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])'
    ) ?? []);
    focusable()[0]?.focus();
    const keydown = (event: KeyboardEvent) => {
      if (event.key === "Escape") { event.preventDefault(); closeRef.current(); return; }
      if (event.key !== "Tab") return;
      const items = focusable();
      if (!items.length) return;
      const first = items[0]; const last = items[items.length - 1];
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
    };
    document.addEventListener("keydown", keydown);
    return () => { document.removeEventListener("keydown", keydown); previousFocus.current?.focus(); };
  }, []);
  return createPortal(
    <div className="modal-backdrop">
      <div ref={panel} className={`modal-panel ${wide ? "modal-panel--wide" : ""}`} role="dialog" aria-modal="true" aria-labelledby={titleId}>
        <header><div><h2 id={titleId}>{title}</h2>{description && <p>{description}</p>}</div><button type="button" className="modal-close" onClick={onClose} aria-label="Close dialog">×</button></header>
        {children}
      </div>
    </div>,
    document.body
  );
}
