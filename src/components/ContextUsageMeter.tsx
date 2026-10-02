import { useEffect, useRef, useState } from "react";
import type { ChangeEvent, KeyboardEvent, PointerEvent } from "react";
import { createPortal } from "react-dom";
import { ChevronRight } from "lucide-react";
import { useI18n } from "../i18n";
import {
  AUTO_COMPACT_MAX_PERCENT,
  AUTO_COMPACT_MIN_PERCENT,
  autoCompactThresholdTokens,
  clampAutoCompactPercent
} from "../lib/autoCompact";
import { computeContextBreakdown, type ContextSegmentId } from "../lib/contextBreakdown";
import { formatCompactTokenCount } from "../lib/contextTokens";
import type { AutoCompactSettings, ContextItem } from "../types";
import { Switch } from "./Common";
import { MenuFlyout, MenuSurfacesContext, createMenuSurfaces } from "./MenuFlyout";
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
  tokens: number;
  /** The meter includes locally estimated components, indicated by `~` in its heading. */
  estimated: boolean;
  /** The current model cannot project context usage. */
  unprojectable?: boolean;
  contextWindow: number | null;
  counts: ContextUsageCounts;
  /** The global auto-compact setting. Without it (and its handler) the panel has no auto-compact row. */
  autoCompact?: AutoCompactSettings;
  onAutoCompactChange?: (next: AutoCompactSettings) => void;
  /**
   * The selected model cannot take a tool mid-conversation, so the handoff tools
   * could never join a run and auto-compact never arms for it. The setting is
   * global and stays as it is; the row says it does not apply here.
   */
  autoCompactUnavailable?: boolean;
}

/**
 * The auto-compact submenu: a switch, and the threshold as a slider with a
 * small number field above it that says the same value exactly.
 *
 * It opens beside its row as `PopoverMenu`'s submenus do, a panel of its own
 * (`MenuFlyout`), and hangs upward from its row, which sits at the foot of a
 * panel that usually opens above the composer. The panel counts a press in it as
 * a press in the panel.
 *
 * Neither control saves while it is being worked: the slider commits when it is
 * released (or on the keyboard's native `change`), the field on Enter or blur,
 * so dragging across the range does not write the document once per step.
 */
function AutoCompactFlyout({
  settings,
  contextWindow,
  onChange
}: {
  settings: AutoCompactSettings;
  contextWindow: number | null;
  onChange: (next: AutoCompactSettings) => void;
}) {
  const { t } = useI18n();
  const [draftPercent, setDraftPercent] = useState(settings.thresholdPercent);
  const [fieldDraft, setFieldDraft] = useState<string | null>(null);
  const committedRef = useRef(settings.thresholdPercent);

  useEffect(() => {
    setDraftPercent(settings.thresholdPercent);
    committedRef.current = settings.thresholdPercent;
  }, [settings.thresholdPercent]);

  const commitPercent = (value: number): void => {
    const next = clampAutoCompactPercent(value);
    setDraftPercent(next);
    setFieldDraft(null);
    if (next === committedRef.current) return;
    committedRef.current = next;
    onChange({ ...settings, thresholdPercent: next });
  };

  const handleSliderChange = (event: ChangeEvent<HTMLInputElement>): void => {
    const next = clampAutoCompactPercent(Number(event.target.value));
    setDraftPercent(next);
    setFieldDraft(null);
    // React reports every step of a drag as onChange; only the native change
    // (keyboard, or a release the browser reports) commits here.
    if (event.nativeEvent.type === "change") commitPercent(next);
  };

  const handleFieldChange = (event: ChangeEvent<HTMLInputElement>): void => {
    const text = event.target.value;
    setFieldDraft(text);
    const value = Number(text);
    // A partial entry ("8" on the way to "85") only moves the slider when it
    // already names a value in range; nothing is saved until Enter or blur.
    if (text.trim() !== "" && Number.isInteger(value)
      && value >= AUTO_COMPACT_MIN_PERCENT && value <= AUTO_COMPACT_MAX_PERCENT) {
      setDraftPercent(value);
    }
  };

  const commitField = (): void => {
    if (fieldDraft === null) return;
    const value = Number(fieldDraft);
    if (fieldDraft.trim() === "" || !Number.isFinite(value)) {
      setFieldDraft(null);
      setDraftPercent(committedRef.current);
      return;
    }
    commitPercent(value);
  };

  const handleFieldKeyDown = (event: KeyboardEvent<HTMLInputElement>): void => {
    if (event.key === "Enter") {
      event.preventDefault();
      commitField();
    }
  };

  const thresholdLabel = t("压缩阈值", "Compaction threshold");
  const disabled = !settings.enabled;
  const thresholdTokens = contextWindow && contextWindow > 0
    ? autoCompactThresholdTokens(contextWindow, draftPercent)
    : null;

  return (
    <MenuFlyout
      role="group"
      aria-label={t("自动压缩", "Auto-compact")}
      className="context-usage-compact__flyout"
      hang="up"
    >
      <div className="context-usage-compact__switch">
        <span>{t("启用自动压缩", "Enable auto-compact")}</span>
        <Switch
          checked={settings.enabled}
          label={t("启用自动压缩", "Enable auto-compact")}
          onChange={(enabled) => onChange({ ...settings, enabled })}
        />
      </div>
      <div className={`context-usage-compact__threshold${disabled ? " context-usage-compact__threshold--disabled" : ""}`}>
        <div className="context-usage-compact__threshold-head">
          <span>{thresholdLabel}</span>
          <span className="context-usage-compact__field">
            <input
              type="number"
              inputMode="numeric"
              aria-label={t("压缩阈值（百分比）", "Compaction threshold (percent)")}
              min={AUTO_COMPACT_MIN_PERCENT}
              max={AUTO_COMPACT_MAX_PERCENT}
              step={1}
              disabled={disabled}
              value={fieldDraft ?? String(draftPercent)}
              onChange={handleFieldChange}
              onKeyDown={handleFieldKeyDown}
              onBlur={commitField}
            />
            <span aria-hidden="true">%</span>
          </span>
        </div>
        <input
          type="range"
          className="context-usage-compact__slider"
          aria-label={thresholdLabel}
          min={AUTO_COMPACT_MIN_PERCENT}
          max={AUTO_COMPACT_MAX_PERCENT}
          step={1}
          disabled={disabled}
          value={draftPercent}
          onInput={(event) => {
            setFieldDraft(null);
            setDraftPercent(clampAutoCompactPercent(Number(event.currentTarget.value)));
          }}
          onChange={handleSliderChange}
          onPointerUp={(event: PointerEvent<HTMLInputElement>) => commitPercent(Number(event.currentTarget.value))}
        />
        <div className="context-usage-compact__scale" aria-hidden="true">
          <span>{AUTO_COMPACT_MIN_PERCENT}%</span>
          <span>{AUTO_COMPACT_MAX_PERCENT}%</span>
        </div>
        <p className="context-usage-compact__hint">
          {thresholdTokens === null
            ? t("当前模型没有设置上下文窗口，无法自动压缩", "The current model has no context window set, so it cannot auto-compact")
            : t(
              "上下文达到 {tokens} tokens 时，模型写好交接文档，在新的交接会话中继续",
              "At {tokens} tokens of context, the model writes handoff notes and continues in a new handover conversation",
              { tokens: formatCompactTokenCount(thresholdTokens) }
            )}
        </p>
      </div>
    </MenuFlyout>
  );
}

/**
 * Context meter at the composer's lower right. The ring opens a breakdown.
 *
 * The ring keeps its fixed footprint without showing unstable numeric text.
 * Its `aria-label` and `title` expose the current value.
 */
export function ContextUsageMeter({
  contexts,
  tokens,
  estimated,
  unprojectable = false,
  contextWindow,
  counts,
  autoCompact,
  onAutoCompactChange,
  autoCompactUnavailable = false
}: ContextUsageMeterProps) {
  const { t } = useI18n();
  const [surfaces] = useState(createMenuSurfaces);
  const { open, position, triggerRef, panelRef, toggle } = usePopoverAnchor({
    align: "end",
    width: PANEL_WIDTH,
    // The auto-compact submenu is a panel of its own on the page, not inside this one.
    keepOpenOnPress: surfaces.contains
  });
  const [compactMenuOpen, setCompactMenuOpen] = useState(false);
  // A reopened panel starts with its submenu closed, as `PopoverMenu` does.
  useEffect(() => {
    if (!open) setCompactMenuOpen(false);
  }, [open]);

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
            zIndex: position?.layer,
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
            {autoCompact?.enabled && !autoCompactUnavailable && breakdown.window !== null && (
              <i
                className="context-usage-panel__threshold"
                style={{ left: `${clampAutoCompactPercent(autoCompact.thresholdPercent)}%` }}
              />
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
          {autoCompact && onAutoCompactChange && (
            <div className="context-usage-compact">
              <button
                type="button"
                className="popover-menu__item context-usage-compact__trigger"
                aria-haspopup="true"
                aria-expanded={compactMenuOpen && !autoCompactUnavailable}
                disabled={autoCompactUnavailable}
                title={autoCompactUnavailable
                  ? t(
                    "当前模型不支持中途追加工具，交接用的工具无法在对话中途加入，所以不会自动压缩。",
                    "The selected model cannot take a tool mid-conversation, so the handoff tools could never join and it does not auto-compact."
                  )
                  : undefined}
                onClick={() => setCompactMenuOpen((current) => !current)}
              >
                <span className="context-usage-compact__label">{t("自动压缩", "Auto-compact")}</span>
                <span className="popover-menu__hint">
                  {autoCompactUnavailable
                    ? t("当前模型不支持", "Not for this model")
                    : autoCompact.enabled
                      ? `${clampAutoCompactPercent(autoCompact.thresholdPercent)}%`
                      : t("已关闭", "Off")}
                </span>
                <ChevronRight
                  size={13}
                  aria-hidden="true"
                  className="popover-menu__chevron"
                />
              </button>
              {compactMenuOpen && !autoCompactUnavailable && (
                <MenuSurfacesContext.Provider value={surfaces}>
                  <AutoCompactFlyout
                    settings={autoCompact}
                    contextWindow={unprojectable ? null : contextWindow}
                    onChange={onAutoCompactChange}
                  />
                </MenuSurfacesContext.Provider>
              )}
            </div>
          )}
        </div>,
        document.body
      )}
    </div>
  );
}
