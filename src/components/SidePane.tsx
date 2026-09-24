import { Maximize2, Minimize2, MoreVertical, X } from "lucide-react";
import { useEffect, useRef } from "react";
import type { ReactNode } from "react";
import { useI18n } from "../i18n";
import { paneKind, sidePaneDomId } from "../lib/sidePanes";
import type { SidePaneId } from "../lib/sidePanes";
import { IconButton } from "./Common";
import { PopoverMenu } from "./PopoverMenu";
import type { PopoverMenuSection } from "./PopoverMenu";
import "./SidePane.css";

export interface SidePaneBounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface SidePaneProps {
  id: SidePaneId;
  title: string;
  /**
   * Takes over the title bar. A pane whose own chrome *is* a title bar — the browser's toolbar —
   * puts it here and the title is never drawn, though it still names the region. Mirrors the
   * reference shell, where a `header` makes the title element unreachable.
   */
  header?: ReactNode;
  /**
   * A second 32px row under the title bar, for chrome the pane owns but that does not
   * belong in the window title: a repository bar, a findings strip. Drawn outside the
   * body, so the published body rectangle stays the box the content actually occupies.
   */
  subheader?: ReactNode;
  /** Pane-specific controls, placed before the pane's own settings menu. */
  trailing?: ReactNode;
  /** Pane-specific menu rows. Given any, the title bar grows a `⋮` settings menu. */
  menuSections?: PopoverMenuSection[];
  /** Whether this pane is currently shown alone; given `onToggleExpand`, drives its icon. */
  expanded?: boolean;
  /** Present only for panes that can be shown alone over the workspace. */
  onToggleExpand?: () => void;
  onClose: () => void;
  /** Any pointer or keyboard entry into the pane; the parent marks it focused. */
  onFocus?: () => void;
  /**
   * Publishes the body rectangle.
   *
   * The native browser page is a child window above the whole renderer, positioned by the host
   * from this exact rectangle. It must be the box that holds `BrowserPanel` and nothing else:
   * the browser's chrome lives in the title bar above, so a rectangle that included the header
   * would slide the page under it.
   */
  onContentBoundsChange?: (bounds: SidePaneBounds) => void;
  children: ReactNode;
}

/** One pseudo-window on the right: a 32px title bar, a close ×, and a measurable body. */
export function SidePane({
  id,
  title,
  header,
  subheader,
  trailing,
  menuSections,
  expanded = false,
  onToggleExpand,
  onClose,
  onFocus,
  onContentBoundsChange,
  children
}: SidePaneProps) {
  const { t } = useI18n();
  const sectionRef = useRef<HTMLElement>(null);
  const bodyRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!onContentBoundsChange) return;
    const body = bodyRef.current;
    const section = sectionRef.current;
    if (!body) return;
    let motionFrame = 0;
    const report = () => {
      const rect = body.getBoundingClientRect();
      onContentBoundsChange({ x: rect.x, y: rect.y, width: rect.width, height: rect.height });
    };
    // A transition moves the box without resizing it, so the observer alone would publish the
    // rectangle the pane had before the animation rather than the one it settles at.
    const reportAfterMotion = (event: Event) => {
      if (event.target !== section) return;
      window.cancelAnimationFrame(motionFrame);
      motionFrame = window.requestAnimationFrame(report);
    };
    report();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(report);
    observer?.observe(body);
    window.addEventListener("resize", report);
    section?.addEventListener("transitionend", reportAfterMotion);
    section?.addEventListener("animationend", reportAfterMotion);
    return () => {
      window.cancelAnimationFrame(motionFrame);
      observer?.disconnect();
      window.removeEventListener("resize", report);
      section?.removeEventListener("transitionend", reportAfterMotion);
      section?.removeEventListener("animationend", reportAfterMotion);
    };
  }, [onContentBoundsChange]);

  return (
    <section
      ref={sectionRef}
      id={sidePaneDomId(id)}
      data-pane-id={id}
      className={`side-pane side-pane--${paneKind(id)}`}
      role="region"
      aria-label={title}
      onPointerDownCapture={onFocus}
      onFocusCapture={onFocus}
    >
      <header className="side-pane__header">
        {header
          ? <div className="side-pane__header-slot">{header}</div>
          : <span className="side-pane__title">{title}</span>}
        <div className="side-pane__controls">
          {trailing}
          {menuSections !== undefined && menuSections.length > 0 && (
            <PopoverMenu
              rootClassName="side-pane__menu"
              triggerClassName="icon-button side-pane__menu-trigger"
              trigger={<MoreVertical size={14} aria-hidden="true" />}
              triggerLabel={t("{name} 设置", "{name} settings", { name: title })}
              menuLabel={t("{name} 设置", "{name} settings", { name: title })}
              sections={menuSections}
              align="end"
              dense
            />
          )}
          {onToggleExpand && (
            <IconButton
              className="side-pane__expand"
              label={expanded ? t("折叠", "Collapse") : t("展开", "Expand")}
              aria-pressed={expanded}
              onMouseDown={(event) => event.preventDefault()}
              onClick={onToggleExpand}
            >
              {expanded
                ? <Minimize2 size={13} aria-hidden="true" />
                : <Maximize2 size={13} aria-hidden="true" />}
            </IconButton>
          )}
          <IconButton
            className="side-pane__close"
            label={t("关闭面板", "Close pane")}
            // Keeps the press from pulling focus out of whatever the pane holds before the click
            // lands, so a page or terminal that is about to be closed never flashes a focus change.
            onMouseDown={(event) => event.preventDefault()}
            onClick={onClose}
          >
            <X size={14} aria-hidden="true" />
          </IconButton>
        </div>
      </header>
      {subheader !== undefined && <div className="side-pane__subheader">{subheader}</div>}
      <div ref={bodyRef} className="side-pane__body">{children}</div>
    </section>
  );
}
