import { Trash2, X } from "lucide-react";
import type { PropsWithChildren, ReactNode } from "react";
import { createContext, useContext, useEffect, useId, useLayoutEffect, useRef, useState } from "react";
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

/** The name of the sidebar window around it, for `DialogSidebarTitle`; null anywhere else. */
const SidebarDialogContext = createContext<{ id: string; title: string } | null>(null);

/**
 * A sidebar window's name, drawn at the head of its sidebar where the title bar used
 * to put it. Outside such a window it draws nothing, so a sidebar that is also used
 * in a side pane (the conversation settings) can carry it unconditionally.
 */
export function DialogSidebarTitle() {
  const dialog = useContext(SidebarDialogContext);
  return dialog && <h2 className="dialog__sidebar-title" id={dialog.id}>{dialog.title}</h2>;
}

/** Whether this is drawn inside a sidebar window, whose selected page names itself at the top. */
export function useInSidebarDialog(): boolean {
  return useContext(SidebarDialogContext) !== null;
}

export function Dialog({
  title,
  description,
  children,
  footer,
  onClose,
  width = "560px",
  dismissible = true,
  sidebar = false,
  bodyClassName,
  className
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
  /**
   * A window laid out as a sidebar of pages beside the selected page, with a flush
   * body. It has no title bar: the name heads the sidebar (the content draws
   * `DialogSidebarTitle` there), the selected page's own title heads the right-hand
   * side, and the close button sits in the top corner beside it.
   */
  sidebar?: boolean;
  /** Added to the scrolling body, for content that brings its own padding and dividers. */
  bodyClassName?: string;
  /** Added to the panel, for a window that needs a size of its own rather than its content's. */
  className?: string;
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

  /* Drawn even where `dismissible` is false. That flag is about closing by
   * ACCIDENT — a stray backdrop click, a reflexive Escape — and says nothing about
   * closing on purpose. A window with no way out in its own corner is one the user
   * has to guess their way out of. */
  const closeButton = (
    <IconButton label={t("关闭", "Close")} onClick={onClose}>
      <X size={16} />
    </IconButton>
  );

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
        className={`dialog${sidebar ? " dialog--sidebar" : ""}${className ? ` ${className}` : ""}`}
        style={{ maxWidth: width }}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={description ? descriptionId : undefined}
        tabIndex={-1}
      >
        {!sidebar && (
          <div className="dialog__header">
            <h2 id={titleId}>{title}</h2>
            {closeButton}
          </div>
        )}
        <SidebarDialogContext.Provider value={sidebar ? { id: titleId, title } : null}>
          <div className={`dialog__body${bodyClassName ? ` ${bodyClassName}` : ""}`}>
            {description && <p className="dialog__lede" id={descriptionId}>{description}</p>}
            {children}
          </div>
        </SidebarDialogContext.Provider>
        {footer && <div className="dialog__footer">{footer}</div>}
        {/* Last, so it is also the last stop of the Tab cycle: the corner it sits
            in is read after the page's own title. */}
        {sidebar && <div className="dialog__close">{closeButton}</div>}
      </div>
    </div>,
    document.body
  );
}

export function EmptyState({ icon, title, description }: { icon: ReactNode; title: string; description?: string }) {
  return (
    <div className="empty-state">
      <div className="empty-state__icon">{icon}</div>
      <h3>{title}</h3>
      {description && <p>{description}</p>}
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
