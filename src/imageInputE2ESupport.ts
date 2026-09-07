import type { ApiProvider, ProviderFamily } from "./types";

// Model-selection support for the image-input E2E runner.
//
// The runner used to switch protocols through `select[aria-label="模型"]`: it read
// `select.options`, wrote a JSON `[providerId, modelId]` value and dispatched `change`. The
// composer has no native select any more — the model picker is a `PopoverMenu` whose panel is
// portaled to `document.body`, whose rows are `button[role="menuitemradio"]`, and whose row
// identity is the visible model id plus the provider name (the React `id` is a key, never a DOM
// value). Driving the old shape found nothing and the run stalled at "等待 模型选择器 超时".
//
// This module holds the interaction and the runner's provider/model naming so the Vitest suite
// exercises the very code the page runs instead of a second, independently correct copy of it.
// It performs no work at import time: no DOM query, no timer, no network.

export const IMAGE_E2E_PROTOCOLS = [
  "openai_chat",
  "openai_responses",
  "anthropic"
] as const satisfies readonly ProviderFamily[];

/** Provider-family subset exercised by the image-input E2E driver. */
export type ImageE2eFamily = (typeof IMAGE_E2E_PROTOCOLS)[number];

export const IMAGE_E2E_DISPLAY_NAMES: Record<ImageE2eFamily, string> = {
  openai_chat: "OpenAI Chat Completions",
  openai_responses: "OpenAI Responses",
  anthropic: "Anthropic Messages"
};

export function imageE2eProviderId(protocol: ImageE2eFamily, runId: string): string {
  return `image-e2e-${protocol}-${runId}`;
}

export function imageE2eModelId(protocol: ImageE2eFamily, runId: string): string {
  return `image-e2e-model-${protocol}-${runId}`;
}

/** The provider name the menu shows as a row's secondary line. */
export function imageE2eProviderName(protocol: ImageE2eFamily): string {
  return `Image E2E · ${IMAGE_E2E_DISPLAY_NAMES[protocol]}`;
}

export function imageE2eProviders(
  { runId, baseUrl }: { runId: string; baseUrl: string }
): ApiProvider[] {
  return IMAGE_E2E_PROTOCOLS.map((protocol) => ({
    id: imageE2eProviderId(protocol, runId),
    name: imageE2eProviderName(protocol),
    enabled: true,
    endpointBaseUrls: {},
    familySettings: {},
    notes: "",
    family: protocol,
    baseUrl,
    models: [{
      id: imageE2eModelId(protocol, runId),
      contextWindow: 128_000,
      maxOutputTokens: 8_192,
      name: "",
      group: "",
      capabilities: ["image_recognition"],
      reasoningContent: protocol === "openai_responses" ? "encrypted" : "plaintext",
      promptCache: true
    }],
    activeModelId: imageE2eModelId(protocol, runId)
  }));
}

/** The two visible strings that identify one row of the model menu. */
export interface ModelMenuTarget {
  /** Primary line of the row: the model id. */
  modelId: string;
  /** Secondary line of the row: the provider name. */
  providerName: string;
}

export function imageE2eModelTarget(
  protocol: ImageE2eFamily,
  runId: string
): ModelMenuTarget {
  return {
    modelId: imageE2eModelId(protocol, runId),
    providerName: imageE2eProviderName(protocol)
  };
}

export interface ModelMenuRow extends ModelMenuTarget {
  element: HTMLButtonElement;
  checked: boolean;
  disabled: boolean;
}

export interface ModelMenuContext {
  /** Where the composer lives. Defaults to `document`. */
  documentRoot?: ParentNode;
  /** Where `PopoverMenu` portals its panel. Defaults to `document.body`. */
  portalRoot?: ParentNode;
  timeoutMs?: number;
  pollMs?: number;
}

/** The composer's model picker root; `App` sets it through `rootClassName`. */
export const MODEL_MENU_ROOT_SELECTOR = ".composer__model";
/** Locale-independent: the trigger is the only `aria-haspopup="menu"` button in that root. */
export const MODEL_MENU_TRIGGER_SELECTOR = 'button[aria-haspopup="menu"]';
export const POPOVER_PANEL_SELECTOR = '.popover-menu__panel[role="menu"]';
export const MODEL_MENU_ITEM_SELECTOR = 'button[role="menuitemradio"]';

const DEFAULT_TIMEOUT_MS = 30_000;
const DEFAULT_POLL_MS = 50;

function sleep(milliseconds: number): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, milliseconds);
  });
}

async function waitUntil(
  label: string,
  probe: () => boolean,
  context: ModelMenuContext,
  diagnose: () => string
): Promise<void> {
  const deadline = Date.now() + (context.timeoutMs ?? DEFAULT_TIMEOUT_MS);
  const pollMs = context.pollMs ?? DEFAULT_POLL_MS;
  while (!probe()) {
    if (Date.now() >= deadline) throw new Error(`等待 ${label} 超时（${diagnose()}）`);
    await sleep(pollMs);
  }
}

export function modelMenuTrigger(context: ModelMenuContext = {}): HTMLButtonElement | null {
  const root = (context.documentRoot ?? document).querySelector(MODEL_MENU_ROOT_SELECTOR);
  return root?.querySelector<HTMLButtonElement>(MODEL_MENU_TRIGGER_SELECTOR) ?? null;
}

/** Every popover panel currently mounted in the portal root. */
export function openPopoverPanels(context: ModelMenuContext = {}): HTMLElement[] {
  return Array.from(
    (context.portalRoot ?? document.body).querySelectorAll<HTMLElement>(POPOVER_PANEL_SELECTOR)
  );
}

function modelMenuOpen(context: ModelMenuContext): boolean {
  return modelMenuTrigger(context)?.getAttribute("aria-expanded") === "true";
}

/**
 * The open model panel, or null.
 *
 * The panel carries no link back to its trigger, so the pairing is proven the only way the DOM
 * allows: our trigger reports itself expanded and exactly one panel is mounted. Two panels means
 * some other popover is also open and the rows below could belong to it.
 */
export function modelMenuPanel(context: ModelMenuContext = {}): HTMLElement | null {
  if (!modelMenuOpen(context)) return null;
  const panels = openPopoverPanels(context);
  return panels.length === 1 ? panels[0] : null;
}

function describeState(context: ModelMenuContext): string {
  const trigger = modelMenuTrigger(context);
  return [
    `触发按钮=${trigger ? "存在" : "缺失"}`,
    `aria-expanded=${trigger?.getAttribute("aria-expanded") ?? "无"}`,
    `已打开面板数=${openPopoverPanels(context).length}`
  ].join("，");
}

export async function openModelMenu(context: ModelMenuContext = {}): Promise<HTMLElement> {
  const trigger = modelMenuTrigger(context);
  if (!trigger) {
    throw new Error(
      `找不到模型选择器：${MODEL_MENU_ROOT_SELECTOR} 内没有 ${MODEL_MENU_TRIGGER_SELECTOR}`
    );
  }
  if (trigger.disabled) throw new Error("模型选择器不可用，无法切换模型");
  if (!modelMenuOpen(context)) trigger.click();
  await waitUntil(
    "模型菜单展开",
    () => modelMenuPanel(context) !== null,
    context,
    () => describeState(context)
  );
  const panel = modelMenuPanel(context);
  if (!panel) throw new Error(`模型菜单展开后又消失了（${describeState(context)}）`);
  return panel;
}

export async function closeModelMenu(context: ModelMenuContext = {}): Promise<void> {
  if (!modelMenuOpen(context)) return;
  modelMenuTrigger(context)?.click();
  await waitUntil(
    "模型菜单收起",
    () => !modelMenuOpen(context),
    context,
    () => describeState(context)
  );
}

export function modelMenuRows(panel: HTMLElement): ModelMenuRow[] {
  return Array.from(panel.querySelectorAll<HTMLButtonElement>(MODEL_MENU_ITEM_SELECTOR))
    .map((element) => ({
      element,
      // textContent rather than innerText: a panel in a hidden preview window — or in jsdom —
      // is never laid out, and innerText reports "" for everything in it.
      modelId: element.querySelector(".popover-menu__copy strong")?.textContent?.trim() ?? "",
      providerName: element.querySelector(".popover-menu__copy small")?.textContent?.trim() ?? "",
      checked: element.getAttribute("aria-checked") === "true",
      disabled: element.disabled
    }));
}

function describeTarget(target: ModelMenuTarget): string {
  return `${target.providerName} · ${target.modelId}`;
}

function describeRows(rows: readonly ModelMenuRow[]): string {
  return rows.length
    ? rows.map((row) => `${describeTarget(row)}${row.disabled ? "（禁用）" : ""}`).join("；")
    : "（空）";
}

/**
 * The single row matching both visible lines.
 *
 * Matching on the model id alone is not enough: two providers may expose the same model id, and
 * picking whichever comes first would silently run the wrong protocol. Absent and disabled
 * targets fail loudly instead of leaving the previous model selected.
 */
export function findModelMenuRow(
  rows: readonly ModelMenuRow[],
  target: ModelMenuTarget
): ModelMenuRow {
  const matches = rows.filter(
    (row) => row.modelId === target.modelId && row.providerName === target.providerName
  );
  if (matches.length === 0) {
    throw new Error(`模型菜单里没有「${describeTarget(target)}」；当前可选：${describeRows(rows)}`);
  }
  if (matches.length > 1) {
    throw new Error(`模型菜单里有 ${matches.length} 个「${describeTarget(target)}」，无法确定选哪一个`);
  }
  const [row] = matches;
  if (row.disabled) throw new Error(`模型菜单里的「${describeTarget(row)}」被禁用，不能沿用当前模型继续`);
  return row;
}

/**
 * Switch the composer to one model and read the selection back out of the menu.
 *
 * The panel closes itself on selection, so waiting for it is what proves the click reached the
 * row's handler. Reopening and reading `aria-checked` is the menu's own account of what is
 * selected; the trigger label alone would also be satisfied by a stale render.
 */
export async function selectModelInMenu(
  target: ModelMenuTarget,
  context: ModelMenuContext = {}
): Promise<void> {
  const panel = await openModelMenu(context);
  findModelMenuRow(modelMenuRows(panel), target).element.click();
  await waitUntil(
    `选择「${describeTarget(target)}」后菜单收起`,
    () => !modelMenuOpen(context),
    context,
    () => describeState(context)
  );

  const reopened = await openModelMenu(context);
  const rows = modelMenuRows(reopened);
  const confirmed = findModelMenuRow(rows, target);
  if (!confirmed.checked) {
    throw new Error(
      `选择「${describeTarget(target)}」后菜单没有把它标为 aria-checked=true；当前选中：${
        describeRows(rows.filter((row) => row.checked))
      }`
    );
  }
  const strays = rows.filter((row) => row.checked && row.element !== confirmed.element);
  if (strays.length) {
    throw new Error(`模型菜单同时标记了多个选中项：${describeRows(strays)}`);
  }
  await closeModelMenu(context);
}
