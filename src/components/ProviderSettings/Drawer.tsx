import { useEffect, useRef } from "react";
import { X } from "lucide-react";
import { useI18n } from "../../i18n";
import { IconButton } from "../Common";

/**
 * Floating drawer on the right.
 *
 * Provider secondary panels use a drawer instead of a centered modal so the provider
 * list remains visible. Its focus trap shares the `DialogShell` implementation.
 */
export function Drawer({
  title,
  subtitle,
  labelledBy,
  width = "normal",
  footer,
  onClose,
  children
}: {
  title: string;
  subtitle?: string;
  labelledBy: string;
  /** Drawer width preset, mapped to `.drawer--<width>`. */
  width?: "normal" | "wide";
  footer?: React.ReactNode;
  onClose: () => void;
  children: React.ReactNode;
}) {
  const { t } = useI18n();
  const panelRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const previous = window.document.activeElement as HTMLElement | null;
    const focusTarget = panelRef.current?.querySelector<HTMLElement>(
      'input:not([disabled]), button:not([disabled])'
    );
    focusTarget?.focus();
    return () => previous?.focus();
  }, []);

  const onDrawerKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    // Prevent drawer keystrokes from reaching settings-page shortcuts.
    event.stopPropagation();
    if (event.key === "Escape") {
      event.preventDefault();
      onClose();
      return;
    }
    if (event.key !== "Tab" || !panelRef.current) return;
    const focusable = Array.from(panelRef.current.querySelectorAll<HTMLElement>(
      'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])'
    ));
    if (!focusable.length) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && window.document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && window.document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  };

  return (
    <div
      className="drawer-backdrop"
      role="presentation"
      onMouseDown={(event) => event.target === event.currentTarget && onClose()}
      onKeyDown={onDrawerKeyDown}
    >
      <div
        ref={panelRef}
        className={`drawer drawer--${width}`}
        role="dialog"
        aria-modal="true"
        aria-labelledby={labelledBy}
        tabIndex={-1}
      >
        <header className="drawer__header">
          <div>
            <h3 id={labelledBy}>{title}</h3>
            {subtitle && <p>{subtitle}</p>}
          </div>
          <IconButton label={t("关闭 {title}", "Close {title}", { title })} onClick={onClose}>
            <X size={16} />
          </IconButton>
        </header>
        <div className="drawer__body">{children}</div>
        {footer && <footer className="drawer__footer">{footer}</footer>}
      </div>
    </div>
  );
}
