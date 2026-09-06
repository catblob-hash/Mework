import { ChevronRight, X } from "lucide-react";
import type { PropsWithChildren, ReactNode } from "react";
import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useI18n } from "../i18n";

export function IconButton({
  label,
  children,
  className = "",
  ...props
}: PropsWithChildren<React.ButtonHTMLAttributes<HTMLButtonElement> & { label: string }>) {
  return (
    <button type="button" className={`icon-button ${className}`} aria-label={label} title={label} data-drag-exclude {...props}>
      {children}
    </button>
  );
}

export function Switch({ checked, onChange, label, disabled = false }: { checked: boolean; onChange: (checked: boolean) => void; label: string; disabled?: boolean }) {
  return (
    <button
      type="button"
      role="switch"
      data-drag-exclude
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      className={`switch ${checked ? "switch--on" : ""}`}
      onClick={() => onChange(!checked)}
    >
      <span />
    </button>
  );
}

export function Dialog({
  title,
  description,
  children,
  footer,
  onClose,
  width = "560px",
  dismissible = true
}: PropsWithChildren<{
  title: string;
  description?: string;
  footer?: ReactNode;
  onClose: () => void;
  width?: string;
  dismissible?: boolean;
}>) {
  const { t } = useI18n();
  const titleId = useId();
  const descriptionId = useId();
  const panelRef = useRef<HTMLDivElement>(null);
  const onCloseRef = useRef(onClose);
  const dismissibleRef = useRef(dismissible);

  useLayoutEffect(() => {
    onCloseRef.current = onClose;
    dismissibleRef.current = dismissible;
  }, [dismissible, onClose]);

  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    panelRef.current?.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      const dialogs = document.querySelectorAll<HTMLElement>('[role="dialog"]');
      if (dialogs.item(dialogs.length - 1) !== panelRef.current) return;
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        if (dismissibleRef.current) onCloseRef.current();
        return;
      }
      if (event.key !== "Tab" || !panelRef.current) return;
      const focusable = Array.from(
        panelRef.current.querySelectorAll<HTMLElement>(
          'button:not([disabled]), input:not([disabled]), textarea:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])'
        )
      );
      if (!focusable.length) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKeyDown, true);
    return () => {
      document.removeEventListener("keydown", onKeyDown, true);
      previous?.focus();
    };
  }, []);

  /* A fixed element is viewport-relative only until an ancestor with transform,
   * filter, perspective, or contain establishes a containing block. Sidebar
   * ancestors use transforms and overflow for collapse and entrance animations,
   * which would clip an in-place dialog. Portal dialogs to `document.body` so
   * they remain viewport-level; React event propagation and component CSS stay
   * unchanged. */
  return createPortal(
    <div
      className="modal-backdrop"
      role="presentation"
      onMouseDown={(event) => {
        if (dismissible && event.target === event.currentTarget) onClose();
      }}
    >
      <div
        ref={panelRef}
        className="dialog"
        style={{ maxWidth: width }}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={description ? descriptionId : undefined}
        tabIndex={-1}
      >
        <div className="dialog__header">
          <div>
            <h2 id={titleId}>{title}</h2>
            {description && <p id={descriptionId}>{description}</p>}
          </div>
          {dismissible && (
            <IconButton label={t("关闭", "Close")} onClick={onClose}>
              <X size={18} />
            </IconButton>
          )}
        </div>
        <div className="dialog__body">{children}</div>
        {footer && <div className="dialog__footer">{footer}</div>}
      </div>
    </div>,
    document.body
  );
}

export function EmptyState({ icon, title, description, action }: { icon: ReactNode; title: string; description: string; action?: ReactNode }) {
  return (
    <div className="empty-state">
      <div className="empty-state__icon">{icon}</div>
      <h3>{title}</h3>
      <p>{description}</p>
      {action}
    </div>
  );
}

export function Field({ label, hint, children }: PropsWithChildren<{ label: string; hint?: string }>) {
  return (
    <label className="field">
      <span className="field__label">{label}</span>
      {children}
      {hint && <span className="field__hint">{hint}</span>}
    </label>
  );
}

export function CheckRow({
  checked,
  onChange,
  title,
  description,
  badge,
  inputAriaLabel
}: {
  checked: boolean;
  onChange: (checked: boolean) => void;
  title: string;
  description?: string;
  badge?: string;
  inputAriaLabel?: string;
}) {
  return (
    <label className="check-row">
      <input
        type="checkbox"
        checked={checked}
        aria-label={inputAriaLabel}
        onChange={(event) => onChange(event.target.checked)}
      />
      <span className="check-row__box" aria-hidden="true" />
      <span className="check-row__copy">
        <span>
          {title}
          {badge && <em>{badge}</em>}
        </span>
        {description && <small>{description}</small>}
      </span>
    </label>
  );
}

/** A collapsible section whose title row is its only heading. It starts closed
 * to keep a new conversation sidebar compact. */
export function CollapsibleSection({
  title,
  summary,
  icon,
  actions,
  defaultOpen = false,
  children
}: PropsWithChildren<{
  title: string;
  summary?: ReactNode;
  icon?: ReactNode;
  /** Persistent controls such as bulk actions. Rendered only while expanded. */
  actions?: ReactNode;
  defaultOpen?: boolean;
}>) {
  const [open, setOpen] = useState(defaultOpen);
  const regionId = useId();
  return (
    <section className={`collapsible-section${open ? " collapsible-section--open" : ""}`}>
      <div className="collapsible-section__heading">
        <button
          type="button"
          className="collapsible-section__toggle"
          aria-expanded={open}
          aria-controls={regionId}
          onClick={() => setOpen((current) => !current)}
        >
          <ChevronRight className={`disclosure-chevron${open ? " disclosure-chevron--open" : ""}`} size={14} />
          {icon}
          <strong>{title}</strong>
          {summary !== undefined && <small>{summary}</small>}
        </button>
        {open && actions}
      </div>
      <div
        id={regionId}
        className={`collapse-region ${open ? "" : "collapse-region--closed"}`}
        aria-hidden={!open || undefined}
        inert={!open || undefined}
      >
        <div className="collapse-region__inner">
          <div className="collapsible-section__body">{children}</div>
        </div>
      </div>
    </section>
  );
}
