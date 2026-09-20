import { ChevronRight, Trash2, X } from "lucide-react";
import type { PropsWithChildren, ReactNode } from "react";
import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useI18n } from "../i18n";
import { useFloatingSurface } from "../lib/floatingSurfaces";

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

/**
 * The delete button that arms itself.
 *
 * One control does what a confirmation dialog used to: the first press turns the
 * trash can into the word "Confirm" without moving or resizing the row, and the
 * second press deletes. Blur and Escape disarm it, so a button armed by a stray
 * click is not left waiting to fire on the next one — which also means only one
 * of these can be armed at a time, because arming a second blurs the first.
 */
export function ConfirmDeleteButton({
  label,
  confirmLabel,
  className = "",
  title,
  disabled = false,
  size = 13,
  onDelete
}: {
  label: string;
  /** What the armed button says it will do. Distinct from `label` so the change is announced. */
  confirmLabel: string;
  className?: string;
  /** The tooltip, when a row has a reason for the button beyond what the label says. */
  title?: string;
  disabled?: boolean;
  size?: number;
  onDelete: () => void;
}) {
  const { t } = useI18n();
  const [armed, setArmed] = useState(false);
  const current = armed ? confirmLabel : label;

  return (
    <IconButton
      label={current}
      title={title ?? current}
      className={`confirm-delete icon-button--danger${armed ? " confirm-delete--armed" : ""}${className ? ` ${className}` : ""}`}
      disabled={disabled}
      onBlur={() => setArmed(false)}
      onKeyDown={(event) => {
        if (event.key !== "Escape" || !armed) return;
        event.preventDefault();
        setArmed(false);
      }}
      /* Rows that are themselves clickable sit under this button, and arming one
         is not a request to open the thing being deleted. */
      onClick={(event) => {
        event.stopPropagation();
        if (!armed) {
          setArmed(true);
          return;
        }
        setArmed(false);
        onDelete();
      }}
    >
      {armed
        ? <span className="confirm-delete__label">{t("确认", "Confirm")}</span>
        : <Trash2 size={size} />}
    </IconButton>
  );
}

/**
 * A text field with no box of its own: it reads as body text until hovered or
 * focused. The wrapper carries a hidden copy of the value so the field grows
 * with its content instead of scrolling inside a fixed frame.
 */
export function PlainField({
  value,
  onChange,
  label,
  placeholder,
  disabled = false,
  autoFocus = false,
  invalid = false,
  className = "",
  onKeyDown,
  onPaste
}: {
  value: string;
  onChange: (value: string) => void;
  label: string;
  placeholder?: string;
  disabled?: boolean;
  autoFocus?: boolean;
  invalid?: boolean;
  className?: string;
  onKeyDown?: React.KeyboardEventHandler<HTMLTextAreaElement>;
  onPaste?: React.ClipboardEventHandler<HTMLTextAreaElement>;
}) {
  return (
    <div className={`plain-field ${invalid ? "plain-field--error" : ""} ${className}`} data-value={value}>
      <textarea
        rows={1}
        value={value}
        aria-label={label}
        aria-invalid={invalid || undefined}
        placeholder={placeholder}
        disabled={disabled}
        autoFocus={autoFocus}
        onChange={(event) => onChange(event.target.value)}
        onKeyDown={onKeyDown}
        onPaste={onPaste}
      />
    </div>
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
  dismissible = true,
  bodyClassName
}: PropsWithChildren<{
  title: string;
  /**
   * The paragraph under the title. It rides at the head of the BODY rather than
   * beside the title, because the header is one line — a name and the way out —
   * and a sentence up there would be the thing deciding how tall every window in
   * the application is.
   *
   * A `dialog__body--flush` body is a whole page laid out as a flex row, so it
   * takes no description: there is no column for a paragraph to lead.
   */
  description?: string;
  footer?: ReactNode;
  onClose: () => void;
  width?: string;
  dismissible?: boolean;
  /** Added to the scrolling body, for content that brings its own padding and dividers. */
  bodyClassName?: string;
}>) {
  const { t } = useI18n();
  const titleId = useId();
  const descriptionId = useId();
  const backdropRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const onCloseRef = useRef(onClose);
  const dismissibleRef = useRef(dismissible);

  // The backdrop, not the panel: a modal owns the whole viewport, so the built-in browser's native
  // page has to go away entirely rather than keep a dialog-shaped hole with live page around it.
  useFloatingSurface(backdropRef, true);

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
      ref={backdropRef}
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
          <h2 id={titleId}>{title}</h2>
          {/* Drawn even where `dismissible` is false. That flag is about closing
              by ACCIDENT — a stray backdrop click, a reflexive Escape — and says
              nothing about closing on purpose. A window with no way out in its
              own corner is one the user has to guess their way out of. */}
          <IconButton label={t("关闭", "Close")} onClick={onClose}>
            <X size={16} />
          </IconButton>
        </div>
        <div className={`dialog__body${bodyClassName ? ` ${bodyClassName}` : ""}`}>
          {description && <p className="dialog__lede" id={descriptionId}>{description}</p>}
          {children}
        </div>
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

export function Field({ label, hint, hintIsError = false, children }: PropsWithChildren<{
  label: string;
  hint?: string;
  /** Colours the hint as a complaint. For a field whose only remaining line under
   * it is the reason a save was refused, rather than standing advice. */
  hintIsError?: boolean;
}>) {
  return (
    <label className="field">
      <span className="field__label">{label}</span>
      {children}
      {hint && (
        <span className={hintIsError ? "field__hint field__hint--error" : "field__hint"}>{hint}</span>
      )}
    </label>
  );
}

export function CheckRow({
  checked,
  onChange,
  title,
  description,
  badge,
  inputAriaLabel,
  disabled = false
}: {
  checked: boolean;
  onChange: (checked: boolean) => void;
  title: string;
  description?: string;
  badge?: string;
  inputAriaLabel?: string;
  disabled?: boolean;
}) {
  return (
    <label className={`check-row${disabled ? " check-row--disabled" : ""}`}>
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
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
