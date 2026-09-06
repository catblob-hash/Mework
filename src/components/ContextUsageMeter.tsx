import { SlidersHorizontal } from "lucide-react";
import { useEffect } from "react";
import { createPortal } from "react-dom";
import { useI18n } from "../i18n";
import { computeContextBreakdown, type ContextSegmentId } from "../lib/contextBreakdown";
import { formatCompactTokenCount } from "../lib/contextTokens";
import type { ContextItem } from "../types";
import { RollingNumber } from "./RollingNumber";
import { usePopoverAnchor } from "./usePopoverAnchor";

const PANEL_WIDTH = 296;
const RING_RADIUS = 6;
const RING_CIRCUMFERENCE = 2 * Math.PI * RING_RADIUS;
/** Meter color thresholds: warn at 70% and alert at 90%. */
const WARNING_RATIO = 0.7;
const DANGER_RATIO = 0.9;

export interface ContextUsageCounts {
  tools: number;
  mcpServers: number;
  skills: number;
  agentRoles: number;
}

export interface ContextUsageMeterProps {
  contexts: ContextItem[];
  systemPrompt: string;
  tokens: number;
  /** The meter includes locally estimated components, indicated by `~` in its heading. */
  estimated: boolean;
  /** The current model cannot project context usage. */
  unprojectable?: boolean;
  contextWindow: number | null;
  counts: ContextUsageCounts;
  onOpenContextSettings: () => void;
  contextSettingsDisabled?: boolean;
}

/**
 * Context meter at the composer's lower right. The ring opens a breakdown.
 *
 * The ring keeps its fixed footprint without showing unstable numeric text.
 * Its `aria-label` and `title` expose the current value.
 *
 * The trigger remains enabled during a run. Only the settings action changes
 * configuration, so it follows the run-state disabled flag.
 */
export function ContextUsageMeter({
  contexts,
  systemPrompt,
  tokens,
  estimated,
  unprojectable = false,
  contextWindow,
  counts,
  onOpenContextSettings,
  contextSettingsDisabled = false
}: ContextUsageMeterProps) {
  const { t } = useI18n();
  const { open, position, triggerRef, panelRef, toggle, close } = usePopoverAnchor({
    align: "end",
    width: PANEL_WIDTH
  });

  // The dialog must receive focus so its action remains reachable by keyboard.
  // Before positioning it is hidden and unfocusable; `preventScroll` avoids
  // triggering the hook's scroll listener, which would immediately close it.
  const positioned = position !== null;
  useEffect(() => {
    if (!open || !positioned) return;
    panelRef.current?.focus({ preventScroll: true });
  }, [open, positioned, panelRef]);

  const breakdown = computeContextBreakdown({
    contexts,
    systemPrompt,
    used: tokens,
    window: unprojectable ? null : contextWindow
  });
  const ratio = unprojectable ? null : breakdown.ratio;
  const tone = ratio === null
    ? ""
    : ratio >= DANGER_RATIO
      ? " context-usage-meter__trigger--danger"
      : ratio >= WARNING_RATIO
        ? " context-usage-meter__trigger--warning"
        : "";

  const prefix = estimated ? "~" : "";
  const headline = unprojectable
    ? t("不可投影", "Not projectable")
    : breakdown.window === null
      ? `${prefix}${formatCompactTokenCount(breakdown.used)}`
      : t(
        "{used} / {window}（{percent}%）",
        "{used} / {window} ({percent}%)",
        {
          used: `${prefix}${formatCompactTokenCount(breakdown.used)}`,
          window: formatCompactTokenCount(breakdown.window),
          percent: Math.round((ratio ?? 0) * 100)
        }
      );

  const segmentLabel = (id: ContextSegmentId): string => {
    if (id === "systemPrompt") return t("系统提示词", "System prompt");
    if (id === "user") return t("用户消息", "User messages");
    if (id === "assistant") return t("助手回复", "Assistant replies");
    if (id === "reasoning") return t("思考", "Reasoning");
    if (id === "tool") return t("工具调用", "Tool calls");
    return t("其他（工具定义等）", "Other (tool schemas, etc.)");
  };
  // Distinguish zero from a nonzero share too small to display at one decimal.
  const formatShare = (share: number): string => (
    share > 0 && share < 0.001 ? "<0.1%" : `${(share * 100).toFixed(1)}%`
  );

  const countItems: { id: string; label: string; value: number }[] = [
    { id: "tools", label: t("工具", "Tools"), value: counts.tools },
    { id: "mcp", label: t("MCP", "MCP"), value: counts.mcpServers },
    { id: "skills", label: t("技能", "Skills"), value: counts.skills },
    { id: "roles", label: t("角色", "Roles"), value: counts.agentRoles }
  ];

  const triggerLabel = unprojectable
    ? t("上下文用量：不可投影", "Context usage: not projectable")
    : t("上下文用量：{headline}", "Context usage: {headline}", { headline });

  return (
    <div className="context-usage-meter">
      <button
        ref={triggerRef}
        type="button"
        data-drag-exclude
        className={`context-usage-meter__trigger${tone}${open ? " context-usage-meter__trigger--open" : ""}`}
        aria-label={triggerLabel}
        title={triggerLabel}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={toggle}
      >
        <svg
          className="context-usage-meter__ring"
          width="16"
          height="16"
          viewBox="0 0 16 16"
          aria-hidden="true"
          focusable="false"
        >
          <circle className="context-usage-meter__track" cx="8" cy="8" r={RING_RADIUS} />
          {ratio !== null && ratio > 0 && (
            <circle
              className="context-usage-meter__fill"
              cx="8"
              cy="8"
              r={RING_RADIUS}
              strokeDasharray={`${RING_CIRCUMFERENCE * ratio} ${RING_CIRCUMFERENCE}`}
            />
          )}
        </svg>
      </button>
      {open && createPortal(
        <div
          ref={panelRef}
          tabIndex={-1}
          className={`popover-menu__panel context-usage-panel${position?.flipped ? " popover-menu__panel--flipped" : ""}`}
          role="dialog"
          aria-label={t("上下文窗口用量", "Context window usage")}
          style={{
            left: position?.left ?? 0,
            top: position?.top ?? 0,
            width: PANEL_WIDTH,
            visibility: position ? "visible" : "hidden"
          }}
        >
          <div className="context-usage-panel__head">
            <span>{t("上下文窗口", "Context window")}</span>
            <strong><RollingNumber value={headline} /></strong>
          </div>
          <div className="context-usage-panel__bar" aria-hidden="true">
            {breakdown.segments.map((segment) => (
              <span
                key={segment.id}
                data-segment={segment.id}
                style={{ flexGrow: segment.tokens }}
              />
            ))}
            {breakdown.free !== null && breakdown.free > 0 && (
              <span data-segment="free" style={{ flexGrow: breakdown.free }} />
            )}
          </div>
          <ul className="context-usage-panel__rows">
            {breakdown.segments.map((segment) => (
              <li key={segment.id}>
                <span className="context-usage-panel__swatch" data-segment={segment.id} />
                <span className="context-usage-panel__name">{segmentLabel(segment.id)}</span>
                <span className="context-usage-panel__tokens">
                  <RollingNumber value={formatCompactTokenCount(segment.tokens)} />
                </span>
                <span className="context-usage-panel__share">
                  <RollingNumber value={formatShare(segment.share)} />
                </span>
              </li>
            ))}
            {breakdown.free !== null && (
              <li>
                <span className="context-usage-panel__swatch" data-segment="free" />
                <span className="context-usage-panel__name">{t("剩余空间", "Free space")}</span>
                <span className="context-usage-panel__tokens">
                  <RollingNumber value={formatCompactTokenCount(breakdown.free)} />
                </span>
                <span className="context-usage-panel__share">
                  <RollingNumber value={formatShare(breakdown.freeShare ?? 0)} />
                </span>
              </li>
            )}
            {!breakdown.segments.length && breakdown.free === null && (
              <li className="context-usage-panel__empty">
                {t("还没有可拆解的上下文", "Nothing to break down yet")}
              </li>
            )}
          </ul>
          <div className="context-usage-panel__counts">
            {countItems.map((item) => (
              <span key={item.id}>
                {item.label}
                <strong>{item.value}</strong>
              </span>
            ))}
          </div>
          <button
            type="button"
            className="context-usage-panel__action"
            disabled={contextSettingsDisabled}
            title={contextSettingsDisabled
              ? t("对话正在运行，暂时不能改设置", "The conversation is running, so settings cannot change yet")
              : undefined}
            onClick={() => {
              close(false);
              onOpenContextSettings();
            }}
          >
            <SlidersHorizontal size={13} />
            <span>{t("打开上下文管理设置", "Open context management settings")}</span>
          </button>
          <p className="context-usage-panel__note">
            {t(
              "成分按本地估算拆分，总量以 provider 报告为准。",
              "Composition is a local estimate; the total comes from the provider."
            )}
          </p>
        </div>,
        document.body
      )}
    </div>
  );
}
