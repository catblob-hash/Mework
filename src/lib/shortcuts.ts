import type { GlobalSettings, KeyToken, ShortcutCommandId, ShortcutPreference } from "../types";

/**
 * Shortcut command catalog.
 *
 * Commands and their defaults are code constants; documents store only user
 * overrides in `GlobalSettings.shortcuts`, so new commands need no schema change
 * and resetting deletes the override.
 *
 * User-visible labels belong in `t()` and are resolved by ID in the settings page.
 */
export type ShortcutGroup = "general" | "conversation" | "message" | "panel";

export interface ShortcutCommand {
  id: ShortcutCommandId;
  group: ShortcutGroup;
  /** Factory binding. An empty array is initially unbound and needs user input. */
  defaultBinding: KeyToken[];
  defaultEnabled: boolean;
  /**
   * False when the application owns this binding. Zoom shortcuts use native
   * browser/WebView bindings and cannot be meaningfully re-recorded.
   */
  editable: boolean;
}

export const SHORTCUT_MODIFIERS = ["Control", "Alt", "Shift", "Meta"] as const;
const MODIFIER_SET = new Set<string>(SHORTCUT_MODIFIERS);

export const SHORTCUT_COMMANDS: readonly ShortcutCommand[] = [
  { id: "app.settings.open", group: "general", defaultBinding: ["Control", "Comma"], defaultEnabled: true, editable: true },
  { id: "app.conversation_settings.open", group: "general", defaultBinding: ["Control", "Shift", "Comma"], defaultEnabled: true, editable: true },
  { id: "app.zoom.in", group: "general", defaultBinding: ["Control", "Equal"], defaultEnabled: true, editable: false },
  { id: "app.zoom.out", group: "general", defaultBinding: ["Control", "Minus"], defaultEnabled: true, editable: false },
  { id: "app.zoom.reset", group: "general", defaultBinding: ["Control", "Digit0"], defaultEnabled: true, editable: false },
  { id: "conversation.create", group: "conversation", defaultBinding: ["Control", "KeyN"], defaultEnabled: true, editable: true },
  { id: "conversation.next", group: "conversation", defaultBinding: ["Control", "Tab"], defaultEnabled: true, editable: true },
  { id: "conversation.previous", group: "conversation", defaultBinding: ["Control", "Shift", "Tab"], defaultEnabled: true, editable: true },
  // Stopping the current run is intentionally unbound by default: it already
  // has a persistent button and a default key could conflict with typing.
  { id: "conversation.stop", group: "conversation", defaultBinding: [], defaultEnabled: false, editable: true },
  { id: "message.copy_last", group: "message", defaultBinding: ["Control", "Shift", "KeyC"], defaultEnabled: false, editable: true },
  { id: "message.edit_last_user", group: "message", defaultBinding: ["Control", "Shift", "KeyE"], defaultEnabled: false, editable: true },
  { id: "panel.browser.toggle", group: "panel", defaultBinding: ["Control", "KeyB"], defaultEnabled: true, editable: true },
  { id: "panel.close", group: "panel", defaultBinding: ["Control", "Shift", "KeyW"], defaultEnabled: false, editable: true }
];

export const SHORTCUT_GROUP_ORDER: readonly ShortcutGroup[] = ["general", "conversation", "message", "panel"];

const COMMANDS_BY_ID = new Map<string, ShortcutCommand>(
  SHORTCUT_COMMANDS.map((command) => [command.id, command])
);

export function shortcutCommand(id: string): ShortcutCommand | undefined {
  return COMMANDS_BY_ID.get(id);
}

export function isModifierToken(token: KeyToken): boolean {
  return MODIFIER_SET.has(token);
}

/** Resolves the current binding from a user override or the code default. */
export function resolveShortcut(
  shortcuts: GlobalSettings["shortcuts"],
  command: ShortcutCommand
): ShortcutPreference {
  const stored = shortcuts[command.id];
  if (!stored) return { binding: [...command.defaultBinding], enabled: command.defaultEnabled };
  return { binding: [...stored.binding], enabled: stored.enabled };
}

/** Returns whether a command differs from its default binding. */
export function isShortcutModified(
  shortcuts: GlobalSettings["shortcuts"],
  command: ShortcutCommand
): boolean {
  const stored = shortcuts[command.id];
  if (!stored) return false;
  return stored.enabled !== command.defaultEnabled
    || stored.binding.length !== command.defaultBinding.length
    || stored.binding.some((token, index) => token !== command.defaultBinding[index]);
}

const NAMED_KEY_LABELS: Record<string, string> = {
  Comma: ",",
  Period: ".",
  Slash: "/",
  Backslash: "\\",
  Semicolon: ";",
  Quote: "'",
  Backquote: "`",
  Minus: "-",
  Equal: "=",
  BracketLeft: "[",
  BracketRight: "]",
  Space: "Space",
  Enter: "Enter",
  Tab: "Tab",
  Escape: "Esc",
  Backspace: "Backspace",
  Delete: "Delete",
  ArrowUp: "↑",
  ArrowDown: "↓",
  ArrowLeft: "←",
  ArrowRight: "→",
  Home: "Home",
  End: "End",
  PageUp: "PgUp",
  PageDown: "PgDn"
};

/** Keycap text for one token, using Windows `Ctrl` / `Alt` / `Shift` / `Win` names. */
export function keyTokenLabel(token: KeyToken): string {
  if (token === "Control") return "Ctrl";
  if (token === "Meta") return "Win";
  if (token === "Alt" || token === "Shift") return token;
  if (token.startsWith("Key")) return token.slice(3);
  if (token.startsWith("Digit")) return token.slice(5);
  if (token.startsWith("Numpad")) return `Num${token.slice(6)}`;
  if (/^F\d{1,2}$/.test(token)) return token;
  return NAMED_KEY_LABELS[token] ?? token;
}

/** Keycaps for a binding, with modifiers in canonical order. */
export function shortcutKeyCaps(binding: readonly KeyToken[]): string[] {
  return orderBinding(binding).map(keyTokenLabel);
}

/** Canonical order: `Control` → `Alt` → `Shift` → `Meta` → terminal key. */
export function orderBinding(binding: readonly KeyToken[]): KeyToken[] {
  const modifiers = SHORTCUT_MODIFIERS.filter((modifier) => binding.includes(modifier));
  const rest = binding.filter((token) => !isModifierToken(token));
  return [...modifiers, ...rest];
}

/**
 * Returns whether a binding is valid.
 *
 * It cannot repeat tokens. It requires at least one modifier plus exactly one
 * terminal key, except a lone `Escape` or function key; a lone letter would
 * trigger while typing.
 */
export function isValidBinding(binding: readonly KeyToken[]): boolean {
  if (!binding.length) return false;
  if (new Set(binding).size !== binding.length) return false;
  const terminals = binding.filter((token) => !isModifierToken(token));
  if (terminals.length !== 1) return false;
  const modifiers = binding.length - 1;
  if (modifiers > 0) return true;
  const [terminal] = terminals;
  return terminal === "Escape" || /^F([1-9]|1\d|2[0-4])$/.test(terminal);
}

/** Returns whether two bindings conflict, ignoring modifier order. */
export function bindingsConflict(left: readonly KeyToken[], right: readonly KeyToken[]): boolean {
  if (!left.length || !right.length || left.length !== right.length) return false;
  const target = new Set(right);
  return left.every((token) => target.has(token));
}

/**
 * Collects a binding from a keydown event using `KeyboardEvent.code`, making it
 * independent of keyboard layout.
 *
 * Returns null until a complete combination is pressed.
 */
export function bindingFromEvent(event: KeyboardEvent): KeyToken[] | null {
  const modifiers: KeyToken[] = [];
  if (event.ctrlKey) modifiers.push("Control");
  if (event.altKey) modifiers.push("Alt");
  if (event.shiftKey) modifiers.push("Shift");
  if (event.metaKey) modifiers.push("Meta");
  const code = event.code;
  if (!code) return null;
  if (/^(Control|Alt|Shift|Meta|OS)(Left|Right)$/.test(code)) return null;
  return orderBinding([...modifiers, code]);
}

/** Returns whether a keydown event matches a binding. */
export function matchesEvent(binding: readonly KeyToken[], event: KeyboardEvent): boolean {
  if (!binding.length) return false;
  const pressed = bindingFromEvent(event);
  return pressed !== null && bindingsConflict(binding, pressed);
}

/**
 * Returns whether a key should be suppressed at the current focus target.
 *
 * Unmodified shortcuts yield to typing in inputs. Ctrl/Alt/Meta combinations
 * still dispatch; a lone Shift is not a modifier because `Shift+A` is uppercase A.
 */
export function shouldSuppressForFocus(binding: readonly KeyToken[], target: EventTarget | null): boolean {
  const hasRealModifier = binding.some(
    (token) => token === "Control" || token === "Alt" || token === "Meta"
  );
  if (hasRealModifier) return false;
  const element = target as HTMLElement | null;
  if (!element || typeof element.closest !== "function") return false;
  return Boolean(element.closest('input, textarea, select, [contenteditable="true"]'));
}
