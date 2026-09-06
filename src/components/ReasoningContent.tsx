import { ChevronRight } from "lucide-react";
import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { useI18n } from "../i18n";
import { useAppearance } from "../lib/appearance";
import { formatTurnDuration } from "../lib/conversationTurns";
import { formatCompactTokenCount } from "../lib/contextTokens";
import { MarkdownContent } from "./MarkdownContent";
import { RollingNumber } from "./RollingNumber";

type ReasoningView = "collapsed" | "preview" | "full";
type ReasoningDetailView = Exclude<ReasoningView, "collapsed">;

interface ReasoningContentProps {
  content: string;
  streaming?: boolean;
  label?: string;
  actions?: ReactNode;
  deferOffscreen?: boolean;
  /** When this round's reasoning opened. Drives the live clock until `durationMs` lands. */
  startedAt?: string;
  /** The provider's own figure. Wins over the live clock the moment it exists. */
  durationMs?: number;
  /** Provider-reported reasoning tokens. Absent or zero renders nothing. */
  tokens?: number;
  /**
   * Whether this reasoning is encrypted, so no body exists locally. Its duration and token count
   * are the only available evidence. Encrypted cards collapse to a static title; plaintext cards
   * remain expandable even when this round produced no text.
   *
   * Callers determine this with `isEncryptedReasoning` from the card's `form`; this component only
   * renders that conclusion. It is required so every caller explicitly classifies the reasoning.
   */
  encrypted: boolean;
}

const COLLAPSE_RELEASE_DELAY_MS = 280;

/** Shared collapsible rendering for model reasoning and generated context summaries. */
export function ReasoningContent({
  content,
  streaming = false,
  label,
  actions,
  deferOffscreen = false,
  startedAt,
  durationMs,
  tokens,
  encrypted
}: ReasoningContentProps) {
  const { t } = useI18n();
  // The appearance preference determines only the initial state. Streaming reasoning begins open;
  // afterwards, an explicit user choice takes precedence.
  const { collapseReasoning } = useAppearance();
  const initiallyExpanded = streaming || !collapseReasoning;
  const [expanded, setExpanded] = useState(initiallyExpanded);
  const [detailView, setDetailView] = useState<ReasoningDetailView>(streaming ? "full" : "preview");
  const [contentMounted, setContentMounted] = useState(initiallyExpanded);
  const [previewOverflows, setPreviewOverflows] = useState(false);
  const [suppressPreviewToggle, setSuppressPreviewToggle] = useState(false);
  const [contentHeights, setContentHeights] = useState({ preview: 0, full: 0 });
  const wasStreaming = useRef(streaming);
  const userChangedViewRef = useRef(false);
  const bodyRef = useRef<HTMLDivElement>(null);
  const toggleRef = useRef<HTMLButtonElement>(null);
  const focusToggleAfterRender = useRef(false);
  const expandedRef = useRef(expanded);
  const detailViewRef = useRef(detailView);
  const streamingRef = useRef(streaming);
  const measureRef = useRef<() => void>(() => undefined);
  const scheduleMeasureRef = useRef<() => void>(() => undefined);
  const contentId = useId();

  // Advance the live clock only while streaming and before the provider reports an authoritative duration.
  const liveClock = streaming && durationMs == null && startedAt !== undefined;
  const [clockNow, setClockNow] = useState(() => Date.now());
  useEffect(() => {
    if (!liveClock) return;
    setClockNow(Date.now());
    const timer = window.setInterval(() => setClockNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [liveClock, startedAt]);

  const startedAtMs = startedAt === undefined ? Number.NaN : Date.parse(startedAt);
  const elapsedMs = durationMs ?? (Number.isFinite(startedAtMs) ? Math.max(0, clockNow - startedAtMs) : undefined);
  const elapsedText = elapsedMs === undefined ? undefined : formatTurnDuration(elapsedMs);
  // Providers report zero rather than omitting tokens when no reasoning occurred.
  const tokenText = tokens ? formatCompactTokenCount(tokens) : undefined;

  // Encrypted reasoning without content is a static row because no body ever reached the client.
  // Plaintext reasoning remains expandable even when empty, while encrypted cards with summaries
  // must expose those summaries. The distinction determines editing permission supplied by callers.
  const expandable = content.length > 0 || !encrypted;

  expandedRef.current = expanded;
  detailViewRef.current = detailView;
  streamingRef.current = streaming;

  useEffect(() => {
    if (streaming && !wasStreaming.current && !userChangedViewRef.current) {
      setContentMounted(true);
      setExpanded(true);
      setDetailView("full");
      setSuppressPreviewToggle(false);
    }
    wasStreaming.current = streaming;
  }, [streaming]);

  useEffect(() => {
    if (expanded) {
      setContentMounted(true);
      return;
    }
    if (!contentMounted) return;
    const timer = window.setTimeout(() => setContentMounted(false), COLLAPSE_RELEASE_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, [contentMounted, expanded]);

  const visibleView: ReasoningView = expanded ? detailView : "collapsed";

  measureRef.current = () => {
    if (!expandedRef.current) return;
    const body = bodyRef.current;
    if (!body) return;
    const style = window.getComputedStyle(body);
    const fontSize = Number.parseFloat(style.fontSize);
    const rawLineHeight = Number.parseFloat(style.lineHeight);
    const lineHeight = style.lineHeight.endsWith("px")
      ? rawLineHeight
      : rawLineHeight * fontSize;
    const previewHeight = Number.isFinite(lineHeight) && lineHeight > 0
      ? lineHeight * 4
      : body.clientHeight;
    const fullHeight = Math.max(body.scrollHeight, body.getBoundingClientRect().height);
    const overflows = fullHeight > previewHeight + 1;
    setContentHeights((current) => (
      Math.abs(current.preview - previewHeight) < 1 && Math.abs(current.full - fullHeight) < 1
        ? current
        : { preview: previewHeight, full: fullHeight }
    ));
    if (!overflows) focusToggleAfterRender.current = false;
    setPreviewOverflows((current) => current === overflows ? current : overflows);
    if (streamingRef.current && detailViewRef.current === "full" && overflows && !userChangedViewRef.current) {
      setSuppressPreviewToggle(false);
      setDetailView("preview");
    }
  };

  useLayoutEffect(() => {
    const body = bodyRef.current;
    if (!body) return;
    let frame: number | null = null;
    const scheduleMeasure = () => {
      if (frame !== null) return;
      frame = window.requestAnimationFrame(() => {
        frame = null;
        measureRef.current();
      });
    };
    scheduleMeasureRef.current = scheduleMeasure;
    const onResize = () => measureRef.current();
    window.addEventListener("resize", onResize);
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(scheduleMeasure);
    observer?.observe(body);
    return () => {
      window.removeEventListener("resize", onResize);
      observer?.disconnect();
      if (frame !== null) window.cancelAnimationFrame(frame);
      scheduleMeasureRef.current = () => undefined;
    };
  }, [contentMounted]);

  useLayoutEffect(() => {
    if (expanded) measureRef.current();
  }, [expanded]);

  useEffect(() => {
    if (expanded) scheduleMeasureRef.current();
  }, [content, expanded]);

  useLayoutEffect(() => {
    if (!focusToggleAfterRender.current || !toggleRef.current) return;
    focusToggleAfterRender.current = false;
    toggleRef.current.focus();
  }, [previewOverflows, suppressPreviewToggle, visibleView]);

  const toggleSuppressed = detailView === "preview" && suppressPreviewToggle;
  // The heading states the round's cost rather than its phase: "Thought for" plus
  // the figures beside it. Only a card with no measured time at all falls back to
  // naming the phase, because there would otherwise be nothing after the verb.
  const headingLabel = label ?? (
    elapsedText === undefined
      ? (streaming ? t("正在思考", "Thinking") : t("思考过程", "Reasoning"))
      : t("思考了", "Thought for")
  );
  // Keep volatile metadata outside the button so it neither changes the accessible name nor repeats.
  const meta = elapsedText === undefined && tokenText === undefined
    ? null
    : (
      <span
        className="reasoning-content__meta"
        title={t("模型思考用时与思考 token 数", "Time and tokens the model spent reasoning")}
      >
        {elapsedText !== undefined && <RollingNumber value={elapsedText} />}
        {elapsedText !== undefined && tokenText !== undefined && " · "}
        {tokenText !== undefined && <RollingNumber value={tokenText} />}
      </span>
    );

  if (!expandable) {
    return (
      <div className="reasoning-content reasoning-content--static" data-state="static">
        <div className="reasoning-content__heading">
          <span className="reasoning-content__summary reasoning-content__summary--static">{headingLabel}</span>
          {meta}
          {actions}
        </div>
      </div>
    );
  }

  return (
    <div className={`reasoning-content reasoning-content--${visibleView}`} data-state={visibleView}>
      <div className="reasoning-content__heading">
        <button
          className="reasoning-content__summary"
          type="button"
          aria-controls={contentMounted ? contentId : undefined}
          aria-expanded={expanded}
          onClick={() => {
            userChangedViewRef.current = true;
            focusToggleAfterRender.current = false;
            setSuppressPreviewToggle(false);
            setDetailView("preview");
            if (!expanded) setContentMounted(true);
            setExpanded((current) => !current);
          }}
        >
          <ChevronRight className={`disclosure-chevron${expanded ? " disclosure-chevron--open" : ""}`} size={15} />
          <span>{headingLabel}</span>
        </button>
        {meta}
        {actions}
      </div>

      <div
        className={`collapse-region reasoning-content__region${expanded ? "" : " collapse-region--closed"}`}
        aria-hidden={!expanded || undefined}
        inert={!expanded || undefined}
      >
        <div className="collapse-region__inner">
          {contentMounted && <div
            id={contentId}
            className={`reasoning-content__panel reasoning-content__panel--${detailView}${detailView === "preview" && previewOverflows ? " reasoning-content__panel--overflowing" : ""}`}
            aria-live={streaming ? "polite" : undefined}
            onPointerMove={() => {
              if (detailView === "preview" && suppressPreviewToggle) setSuppressPreviewToggle(false);
            }}
            onPointerLeave={() => {
              if (detailView === "preview" && suppressPreviewToggle) setSuppressPreviewToggle(false);
            }}
          >
            <div
              className="reasoning-content__viewport"
              style={contentHeights.full > 0 ? {
                maxHeight: `${detailView === "full" ? contentHeights.full : contentHeights.preview}px`
              } : undefined}
            >
              <div ref={bodyRef} className="reasoning-content__body">
                <MarkdownContent content={content} deferOffscreen={deferOffscreen} streaming={streaming} />
              </div>
            </div>
            {previewOverflows && (
              <button
                key={detailView}
                ref={toggleRef}
                type="button"
                className={`reasoning-content__toggle reasoning-content__toggle--${detailView === "full" ? "collapse" : "expand"}${toggleSuppressed ? " reasoning-content__toggle--suppressed" : ""}`}
                aria-hidden={toggleSuppressed || undefined}
                tabIndex={toggleSuppressed ? -1 : undefined}
                onClick={(event) => {
                  userChangedViewRef.current = true;
                  const pointerInitiated = event.detail > 0;
                  focusToggleAfterRender.current = !pointerInitiated;
                  if (pointerInitiated) event.currentTarget.blur();
                  if (detailView === "full") {
                    setSuppressPreviewToggle(pointerInitiated);
                    setDetailView("preview");
                  } else {
                    setSuppressPreviewToggle(false);
                    setDetailView("full");
                  }
                }}
              >
                {detailView === "full" ? t("收起", "Collapse") : t("展开", "Expand")}
              </button>
            )}
          </div>}
        </div>
      </div>
    </div>
  );
}
