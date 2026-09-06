import { ArrowLeft } from "lucide-react";
import { useEffect, useRef } from "react";
import type { ReactNode } from "react";
import { useI18n } from "../i18n";
import "./TaskPage.css";

export interface TaskPageBounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface TaskPageProps {
  /** Only the active page is visible; the rest stay mounted and inert. */
  active: boolean;
  /** Small uppercase label above the title — what kind of page this is. */
  eyebrow: string;
  title: string;
  domId?: string;
  /** Controls on the trailing edge of the header: a stop button, a status pill. */
  actions?: ReactNode;
  /**
   * Shown when the conversation needs the user while this page is covering it.
   *
   * The approval and question docks live inside the conversation pane, which is hidden while a
   * page is up. The right sidebar never had this problem — it sat beside the conversation — so
   * without this banner a model blocked on approval would simply look stuck.
   */
  attention?: { label: string; onGoBack: () => void } | null;
  onBack: () => void;
  /**
   * Publishes the content rectangle while this page is active.
   *
   * The native browser page is a child window above the whole renderer, positioned by the host
   * from this exact rectangle. It must be the box that holds `BrowserPanel` and nothing else:
   * the host reserves `occludedTop` inside it for the browser's own chrome, so measuring any
   * larger container would slide the page under the trusted UI above it.
   */
  onContentBoundsChange?: (bounds: TaskPageBounds) => void;
  children: ReactNode;
}

/**
 * One page in the message area: preview, read-only shell output, git review.
 *
 * This is what is left of the right sidebar once the tabs, the add menu, the launcher home and
 * the width drag are gone. Navigation is the task bar's job now, so all a page needs is a way
 * back and — for the browser — an honest rectangle.
 */
export function TaskPage({
  active,
  eyebrow,
  title,
  domId,
  actions,
  attention = null,
  onBack,
  onContentBoundsChange,
  children
}: TaskPageProps) {
  const { t } = useI18n();
  const sectionRef = useRef<HTMLElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!active || !onContentBoundsChange) return;
    const content = contentRef.current;
    const section = sectionRef.current;
    if (!content) return;
    let motionFrame = 0;
    const report = () => {
      const rect = content.getBoundingClientRect();
      onContentBoundsChange({ x: rect.x, y: rect.y, width: rect.width, height: rect.height });
    };
    // A transition moves the box without resizing it, so the observer alone would publish the
    // rectangle the page had before the animation rather than the one it settles at.
    const reportAfterMotion = (event: Event) => {
      if (event.target !== section) return;
      window.cancelAnimationFrame(motionFrame);
      motionFrame = window.requestAnimationFrame(report);
    };
    report();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(report);
    observer?.observe(content);
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
  }, [active, onContentBoundsChange]);

  return (
    <section
      ref={sectionRef}
      id={domId}
      className="task-page"
      aria-label={t("{title}页面", "{title} page", { title })}
      hidden={!active}
      inert={!active || undefined}
    >
      <header className="task-page__header">
        <button
          type="button"
          className="task-page__back"
          aria-label={t("返回对话", "Back to the conversation")}
          onClick={onBack}
        >
          <ArrowLeft size={16} aria-hidden="true" />
        </button>
        <div className="task-page__heading">
          <span>{eyebrow}</span>
          <h2>{title}</h2>
        </div>
        {actions ? <div className="task-page__actions">{actions}</div> : null}
      </header>
      {attention && (
        <button
          type="button"
          className="task-page__attention"
          onClick={attention.onGoBack}
        >
          <span>{attention.label}</span>
          <span className="task-page__attention-action">{t("返回对话", "Back to the conversation")}</span>
        </button>
      )}
      <div ref={contentRef} className="task-page__content">{children}</div>
    </section>
  );
}
