import {
  Check,
  MoreHorizontal,
  RotateCcw,
  Search,
  X
} from "lucide-react";
import type { JSX, KeyboardEvent as ReactKeyboardEvent } from "react";
import { useEffect, useRef, useState } from "react";
import { useI18n } from "../../i18n";
import {
  bindingsConflict,
  bindingFromEvent,
  isShortcutModified,
  isValidBinding,
  keyTokenLabel,
  orderBinding,
  resolveShortcut,
  SHORTCUT_COMMANDS,
  SHORTCUT_GROUP_ORDER,
  shortcutKeyCaps
} from "../../lib/shortcuts";
import type { ShortcutCommand, ShortcutGroup } from "../../lib/shortcuts";
import type {
  GlobalSettings,
  KeyToken,
  ShortcutCommandId,
  ShortcutPreference
} from "../../types";
import { IconButton, Switch } from "../Common";
import { SettingsPageHeading } from "../SettingsPageHeading";
import "./ShortcutSettings.css";

type Shortcuts = GlobalSettings["shortcuts"];
type GroupFilter = "all" | ShortcutGroup;

interface ConflictNotice {
  commandId: ShortcutCommandId;
  conflictingId: ShortcutCommandId;
}

function updatePreference(
  shortcuts: Shortcuts,
  command: ShortcutCommand,
  preference: ShortcutPreference
): Shortcuts {
  const next = { ...shortcuts };
  const matchesDefault = preference.enabled === command.defaultEnabled
    && preference.binding.length === command.defaultBinding.length
    && preference.binding.every((token, index) => token === command.defaultBinding[index]);
  if (matchesDefault) delete next[command.id];
  else next[command.id] = { binding: [...preference.binding], enabled: preference.enabled };
  return next;
}

function findConflict(
  shortcuts: Shortcuts,
  commandId: ShortcutCommandId,
  binding: readonly KeyToken[]
): ShortcutCommand | null {
  return SHORTCUT_COMMANDS.find((other) => {
    if (other.id === commandId) return false;
    const resolved = resolveShortcut(shortcuts, other);
    return resolved.enabled && bindingsConflict(binding, resolved.binding);
  }) ?? null;
}

function heldModifiers(event: ReactKeyboardEvent<HTMLButtonElement>): KeyToken[] {
  const modifiers: KeyToken[] = [];
  if (event.ctrlKey) modifiers.push("Control");
  if (event.altKey) modifiers.push("Alt");
  if (event.shiftKey) modifiers.push("Shift");
  if (event.metaKey) modifiers.push("Meta");
  return orderBinding(modifiers);
}

export function ShortcutSettings({
  shortcuts,
  onChange
}: {
  shortcuts: GlobalSettings["shortcuts"];
  onChange: (shortcuts: GlobalSettings["shortcuts"]) => void;
}): JSX.Element {
  const { t } = useI18n();
  const [searchOpen, setSearchOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [group, setGroup] = useState<GroupFilter>("all");
  const [menuOpen, setMenuOpen] = useState(false);
  const [recordingId, setRecordingId] = useState<ShortcutCommandId | null>(null);
  const [pendingBinding, setPendingBinding] = useState<KeyToken[]>([]);
  const [conflictNotice, setConflictNotice] = useState<ConflictNotice | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const recordingRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const conflictTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const commandLabels: Record<ShortcutCommandId, string> = {
    "app.settings.open": t("打开设置", "Open settings"),
    "app.conversation_settings.open": t("打开本对话设置", "Open conversation settings"),
    "app.zoom.in": t("放大", "Zoom in"),
    "app.zoom.out": t("缩小", "Zoom out"),
    "app.zoom.reset": t("重置缩放", "Reset zoom"),
    "conversation.create": t("新建对话", "New conversation"),
    "conversation.next": t("下一个对话", "Next conversation"),
    "conversation.previous": t("上一个对话", "Previous conversation"),
    "conversation.stop": t("停止当前运行", "Stop current run"),
    "message.copy_last": t("复制最后一条消息", "Copy last message"),
    "message.edit_last_user": t("编辑最后一条用户消息", "Edit last user message"),
    "panel.browser.toggle": t("打开或关闭内置浏览器", "Toggle built-in browser"),
    "panel.close": t("关闭侧边面板", "Close side panel")
  };
  const groupLabels: Record<ShortcutGroup, string> = {
    general: t("通用", "General"),
    conversation: t("对话", "Conversation"),
    message: t("消息", "Messages"),
    panel: t("面板", "Panels")
  };

  const normalizedQuery = query.trim().toLocaleLowerCase();
  const searchMatches = SHORTCUT_COMMANDS.filter((command) => {
    if (!normalizedQuery) return true;
    const caps = shortcutKeyCaps(resolveShortcut(shortcuts, command).binding);
    const searchableCaps = `${caps.join(" ")} ${caps.join("+")}`.toLocaleLowerCase();
    return commandLabels[command.id].toLocaleLowerCase().includes(normalizedQuery)
      || searchableCaps.includes(normalizedQuery);
  });
  const groupCounts = new Map<ShortcutGroup, number>(
    SHORTCUT_GROUP_ORDER.map((item) => [
      item,
      searchMatches.filter((command) => command.group === item).length
    ])
  );
  const selectedGroupCount = group === "all" ? searchMatches.length : (groupCounts.get(group) ?? 0);
  const visibleCommands = searchMatches.filter((command) => group === "all" || command.group === group);

  useEffect(() => {
    if (searchOpen) searchRef.current?.focus();
  }, [searchOpen]);

  // The recording button is conditionally rendered; depend on the stable command id so
  // new parent records do not steal focus on every render.
  useEffect(() => {
    if (recordingId) recordingRef.current?.focus();
  }, [recordingId]);

  useEffect(() => {
    if (group !== "all" && selectedGroupCount === 0) setGroup("all");
  }, [group, selectedGroupCount]);

  useEffect(() => {
    if (!menuOpen) return;
    const closeOutside = (event: MouseEvent) => {
      if (!menuRef.current?.contains(event.target as Node)) setMenuOpen(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setMenuOpen(false);
    };
    document.addEventListener("mousedown", closeOutside);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("mousedown", closeOutside);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [menuOpen]);

  useEffect(() => () => {
    if (conflictTimerRef.current) clearTimeout(conflictTimerRef.current);
  }, []);

  const showConflict = (commandId: ShortcutCommandId, conflictingId: ShortcutCommandId) => {
    if (conflictTimerRef.current) clearTimeout(conflictTimerRef.current);
    setConflictNotice({ commandId, conflictingId });
    conflictTimerRef.current = setTimeout(() => {
      setConflictNotice(null);
      conflictTimerRef.current = null;
    }, 2000);
  };

  const cancelRecording = () => {
    setRecordingId(null);
    setPendingBinding([]);
  };

  const handleRecordingKeyDown = (
    event: ReactKeyboardEvent<HTMLButtonElement>,
    command: ShortcutCommand
  ) => {
    event.preventDefault();
    event.stopPropagation();
    if (event.nativeEvent.isComposing || event.key === "Process") return;

    const escapeAlone = event.key === "Escape"
      && !event.ctrlKey
      && !event.altKey
      && !event.shiftKey
      && !event.metaKey;
    if (escapeAlone) {
      cancelRecording();
      return;
    }

    const candidate = bindingFromEvent(event.nativeEvent);
    if (candidate === null) {
      setPendingBinding(heldModifiers(event));
      return;
    }
    const ordered = orderBinding(candidate);
    setPendingBinding(ordered);
    if (!isValidBinding(ordered)) return;

    const conflict = findConflict(shortcuts, command.id, ordered);
    if (conflict) {
      showConflict(command.id, conflict.id);
      return;
    }
    onChange(updatePreference(shortcuts, command, { binding: ordered, enabled: true }));
    setConflictNotice(null);
    cancelRecording();
  };

  const handleEnabledChange = (command: ShortcutCommand, enabled: boolean) => {
    const resolved = resolveShortcut(shortcuts, command);
    if (enabled) {
      const conflict = findConflict(shortcuts, command.id, resolved.binding);
      if (conflict) {
        showConflict(command.id, conflict.id);
        return;
      }
    }
    onChange(updatePreference(shortcuts, command, { ...resolved, enabled }));
  };

  const setVisibleEnabled = (enabled: boolean) => {
    let proposed = shortcuts;
    for (const command of visibleCommands) {
      const resolved = resolveShortcut(proposed, command);
      if (!resolved.binding.length) continue;
      proposed = updatePreference(proposed, command, { ...resolved, enabled });
    }

    if (enabled) {
      // Check the complete proposed state before writing anything back to prevent a partial bulk update.
      const visibleIds = new Set(visibleCommands.map((command) => command.id));
      const enabledCommands = SHORTCUT_COMMANDS.filter((command) => resolveShortcut(proposed, command).enabled);
      for (let index = 0; index < enabledCommands.length; index += 1) {
        const left = enabledCommands[index];
        for (let otherIndex = index + 1; otherIndex < enabledCommands.length; otherIndex += 1) {
          const right = enabledCommands[otherIndex];
          if (!visibleIds.has(left.id) && !visibleIds.has(right.id)) continue;
          if (!bindingsConflict(
            resolveShortcut(proposed, left).binding,
            resolveShortcut(proposed, right).binding
          )) continue;
          const target = visibleIds.has(left.id) ? left : right;
          const conflict = target.id === left.id ? right : left;
          showConflict(target.id, conflict.id);
          return;
        }
      }
    }
    onChange(proposed);
  };

  const renderCaps = (binding: readonly KeyToken[]) => shortcutKeyCaps(binding).map((cap) => (
    <kbd key={cap}>{cap}</kbd>
  ));

  const headingActions = (
    <div className="shortcut-settings-page__actions">
      {searchOpen || query ? (
        <div className="shortcut-settings-page__search">
          <Search size={13} aria-hidden="true" />
          <input
            ref={searchRef}
            value={query}
            aria-label={t("搜索快捷键", "Search shortcuts")}
            placeholder={t("搜索快捷键", "Search shortcuts")}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key !== "Escape") return;
              event.stopPropagation();
              setQuery("");
              setSearchOpen(false);
            }}
          />
          <IconButton
            label={t("收起搜索", "Collapse search")}
            className="shortcut-settings-page__search-close"
            onClick={() => {
              setQuery("");
              setSearchOpen(false);
            }}
          >
            <X size={12} />
          </IconButton>
        </div>
      ) : (
        <IconButton
          label={t("搜索快捷键", "Search shortcuts")}
          className="shortcut-settings-page__action-button"
          onClick={() => setSearchOpen(true)}
        >
          <Search size={15} />
        </IconButton>
      )}
      <select
        className="shortcut-settings-page__group-select"
        aria-label={t("筛选快捷键分组", "Filter shortcut groups")}
        value={group}
        onChange={(event) => setGroup(event.target.value as GroupFilter)}
      >
        <option value="all">{t("全部（{count}）", "All ({count})", { count: searchMatches.length })}</option>
        {SHORTCUT_GROUP_ORDER.filter((item) => (groupCounts.get(item) ?? 0) > 0).map((item) => (
          <option key={item} value={item}>
            {t("{name}（{count}）", "{name} ({count})", {
              name: groupLabels[item],
              count: groupCounts.get(item) ?? 0
            })}
          </option>
        ))}
      </select>
      <div className="shortcut-settings-page__menu-wrap" ref={menuRef}>
        <IconButton
          label={t("更多快捷键操作", "More shortcut actions")}
          className="shortcut-settings-page__action-button"
          aria-haspopup="menu"
          aria-expanded={menuOpen}
          onClick={() => setMenuOpen((current) => !current)}
        >
          <MoreHorizontal size={16} />
        </IconButton>
        {menuOpen && (
          <div className="shortcut-settings-page__menu" role="menu">
            <button
              type="button"
              role="menuitem"
              onClick={() => {
                setMenuOpen(false);
                setVisibleEnabled(true);
              }}
            >
              <Check size={13} />
              {t("全部启用", "Enable all")}
            </button>
            <button
              type="button"
              role="menuitem"
              onClick={() => {
                setMenuOpen(false);
                setVisibleEnabled(false);
              }}
            >
              <X size={13} />
              {t("全部停用", "Disable all")}
            </button>
            <hr className="shortcut-settings-page__menu-divider" />
            <button
              type="button"
              role="menuitem"
              className="shortcut-settings-page__menu-danger"
              onClick={() => {
                setMenuOpen(false);
                cancelRecording();
                onChange({});
              }}
            >
              <RotateCcw size={13} />
              {t("重置为默认值", "Reset to defaults")}
            </button>
          </div>
        )}
      </div>
    </div>
  );

  return (
    <section className="settings-page shortcut-settings-page">
      <SettingsPageHeading
        title={t("快捷键", "Keyboard shortcuts")}
        description={t("查看和自定义应用快捷键。", "View and customize application shortcuts.")}
        action={headingActions}
      />
      <div className="settings-card shortcut-settings-page__card">
        {visibleCommands.length ? visibleCommands.map((command) => {
          const resolved = resolveShortcut(shortcuts, command);
          const label = commandLabels[command.id];
          const empty = resolved.binding.length === 0;
          const recording = recordingId === command.id;
          const conflict = conflictNotice?.commandId === command.id
            ? commandLabels[conflictNotice.conflictingId]
            : null;
          return (
            <div
              key={command.id}
              className={`shortcut-settings-page__row${resolved.enabled ? "" : " shortcut-settings-page__row--disabled"}`}
            >
              <div className="shortcut-settings-page__label">
                {isShortcutModified(shortcuts, command) && (
                  <IconButton
                    label={t("撤销「{name}」的修改", "Undo changes to “{name}”", { name: label })}
                    className="shortcut-settings-page__undo"
                    onClick={() => {
                      const next = { ...shortcuts };
                      delete next[command.id];
                      onChange(next);
                    }}
                  >
                    <RotateCcw size={12} />
                  </IconButton>
                )}
                <span>{label}</span>
              </div>
              <div className="shortcut-settings-page__binding-cell">
                {recording ? (
                  <button
                    ref={recordingRef}
                    type="button"
                    className="shortcut-settings-page__recording"
                    aria-label={t("为「{name}」录制快捷键", "Record shortcut for “{name}”", { name: label })}
                    onKeyDown={(event) => handleRecordingKeyDown(event, command)}
                    onBlur={cancelRecording}
                  >
                    <span>{t("按下快捷键", "Press shortcut")}</span>
                    {pendingBinding.length > 0 && (
                      <span className="shortcut-settings-page__pending-caps">
                        {pendingBinding.map((token) => (
                          <kbd key={token}>{keyTokenLabel(token)}</kbd>
                        ))}
                      </span>
                    )}
                  </button>
                ) : (
                  <button
                    type="button"
                    disabled={!command.editable}
                    className={`shortcut-settings-page__binding${empty ? " shortcut-settings-page__binding--empty" : ""}`}
                    aria-label={command.editable
                      ? t("修改「{name}」的快捷键", "Change shortcut for “{name}”", { name: label })
                      : t("「{name}」的快捷键不可修改", "The shortcut for “{name}” cannot be changed", { name: label })}
                    title={command.editable
                      ? t("点击后按下新的组合键", "Click, then press a new key combination")
                      : t("此快捷键由应用保留，不能修改", "This shortcut is reserved by the application")}
                    onClick={() => {
                      setConflictNotice(null);
                      setPendingBinding([]);
                      setRecordingId(command.id);
                    }}
                  >
                    {empty ? <span>{t("按下快捷键", "Press shortcut")}</span> : renderCaps(resolved.binding)}
                  </button>
                )}
                {conflict && (
                  <span className="shortcut-settings-page__conflict" role="alert">
                    {t("与「{name}」冲突", "Conflicts with “{name}”", { name: conflict })}
                  </span>
                )}
              </div>
              <span
                className="shortcut-settings-page__switch-wrap"
                title={empty
                  ? t("先绑定一个组合键才能改启用状态", "Bind a key combination before changing its enabled state")
                  : undefined}
              >
                <Switch
                  checked={resolved.enabled}
                  disabled={empty}
                  label={t("启用「{name}」", "Enable “{name}”", { name: label })}
                  onChange={(enabled) => handleEnabledChange(command, enabled)}
                />
              </span>
            </div>
          );
        }) : (
          <div className="shortcut-settings-page__empty">
            {t("这个分组里没有快捷键", "There are no shortcuts in this group")}
          </div>
        )}
      </div>
    </section>
  );
}
