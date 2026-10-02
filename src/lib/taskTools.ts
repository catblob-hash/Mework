import { MEMORY_TOOL_NAME_SET } from "./memoryTools";

/**
 * Catalog names for task-runtime tools. They are host-derived, not user settings:
 * conversations that produce tasks must also be able to list and await them, and
 * `box` is the carrier of every message the host appends — an unawaited task's
 * result, a hook's context, a skill added later — so every run declares it.
 * Keep descriptors for timeline rendering, but exclude them from selection and
 * persisted enabled lists. Mirrors Rust `agents::TASK_RUNTIME_TOOL_NAMES`.
 */
const TASK_RUNTIME_TOOL_NAMES = ["task_wait", "task_list", "box"] as const;

const TASK_RUNTIME_TOOL_NAME_SET: ReadonlySet<string> = new Set(TASK_RUNTIME_TOOL_NAMES);

/**
 * The preview tools that bring the conversation's page into existence.
 *
 * `preview_start` points the page at the server it just started; the page tools
 * act on that page and mint it when it does not exist yet. `preview_stop`,
 * `preview_list` and `preview_logs` only read or kill a process, so they never
 * produce a page and are deliberately absent.
 *
 * This is a page roster, not a task roster: a page is a view of the dev server
 * and has no task address of its own. Only `preview_start` appears in Rust
 * `agents::TASK_PRODUCING_TOOL_NAMES`, because only it starts the process a
 * `preview:<serverId>` row stands for.
 */
const PREVIEW_PAGE_TOOL_NAMES = [
  "preview_start",
  "preview_console_logs",
  "preview_screenshot",
  "preview_snapshot",
  "preview_inspect",
  "preview_click",
  "preview_fill",
  "preview_eval",
  "preview_network",
  "preview_resize",
  "preview_upload_image",
  "preview_dialog"
] as const;

const PREVIEW_PAGE_TOOL_NAME_SET: ReadonlySet<string> = new Set(PREVIEW_PAGE_TOOL_NAMES);

export function isPreviewPageToolName(name: string): boolean {
  return PREVIEW_PAGE_TOOL_NAME_SET.has(name);
}

/**
 * Every catalog tool that acts on the conversation's preview, page tools and
 * server tools alike.
 *
 * The prefix is the whole test rather than a list, so a tool added to the
 * catalog is covered the day it lands: none of them is a call the timeline
 * offers to place.
 */
export function isPreviewToolName(name: string): boolean {
  return name.startsWith("preview_");
}

/**
 * The preview tools that start, stop and list dev servers.
 *
 * The tool picker has no row for them: they are on exactly while any other
 * preview tool is, because every other one needs a server to have been started
 * and a way to find or stop it again, and none of the three does anything for
 * a model that has no other preview tool.
 */
const PREVIEW_LIFECYCLE_TOOL_NAMES = ["preview_start", "preview_stop", "preview_list"] as const;

const PREVIEW_LIFECYCLE_TOOL_NAME_SET: ReadonlySet<string> = new Set(PREVIEW_LIFECYCLE_TOOL_NAMES);

export function isPreviewLifecycleToolName(name: string): boolean {
  return PREVIEW_LIFECYCLE_TOOL_NAME_SET.has(name);
}

/**
 * `enabledTools` with the preview lifecycle tools brought in step with the
 * other preview tools: each one in `available` added while any other preview
 * tool is on, and all of them taken off otherwise.
 */
export function withPreviewLifecycleTools(
  enabledTools: readonly string[],
  available: ReadonlySet<string>
): string[] {
  const previewOn = enabledTools.some(
    (name) => isPreviewToolName(name) && !isPreviewLifecycleToolName(name)
  );
  if (!previewOn) return enabledTools.filter((name) => !isPreviewLifecycleToolName(name));
  return Array.from(new Set([
    ...enabledTools,
    ...PREVIEW_LIFECYCLE_TOOL_NAMES.filter((name) => available.has(name))
  ]));
}

/**
 * Catalog name for the on-demand skill tool. It is host-derived from
 * `skillToolEnabled` and the number of resolved skills.
 */
const SKILL_TOOL_NAME = "skill";

/**
 * Catalog name for on-demand MCP tool loading. Host-derived from
 * `mcpToolDiscoveryEnabled` and the number of tools a run actually withheld.
 * Mirrors Rust `capabilities::TOOL_SEARCH_TOOL`.
 */
const TOOL_SEARCH_TOOL_NAME = "tool_search";

/**
 * Catalog names for the plan-mode tools. The host derives them from the
 * conversation's Plan mode switch — on offers `plan` and `exit_plan_mode`, off
 * offers neither — so neither is a row in the tool picker. Mirrors the Rust
 * plan tool names.
 */
const PLAN_TOOL_NAMES = ["plan", "exit_plan_mode"] as const;

const PLAN_TOOL_NAME_SET: ReadonlySet<string> = new Set(PLAN_TOOL_NAMES);

/**
 * Catalog names for the handoff tools. The host derives them for itself once
 * the conversation's context crosses the auto-compact threshold (and offers
 * `read_handoff_note` to a conversation that inherited notes), so none is a
 * user setting. Mirrors Rust `handoff::TOOL_NAMES`.
 */
const HANDOFF_TOOL_NAMES = [
  "read_handoff_note",
  "create_handoff_note",
  "edit_handoff_note",
  "handoff"
] as const;

const HANDOFF_TOOL_NAME_SET: ReadonlySet<string> = new Set(HANDOFF_TOOL_NAMES);

/**
 * Catalog names for the two web tools. They follow the conversation's single
 * web-access switch, not two checkboxes, because upstreams do not agree on how
 * many web tools there are: DeepSeek and OpenAI expose search alone and keep
 * page retrieval inside it, while Anthropic exposes search and fetch separately.
 * The host decides which of the pair a run gets from the resolved backend, so
 * the picker offering them individually would promise a shape the upstream may
 * not have. Mirrors Rust `web_search::WEB_TOOL_NAMES`.
 */
const WEB_TOOL_NAMES = ["web_search", "web_fetch"] as const;

const WEB_TOOL_NAME_SET: ReadonlySet<string> = new Set(WEB_TOOL_NAMES);

export function isWebToolName(name: string): boolean {
  return WEB_TOOL_NAME_SET.has(name);
}

/**
 * Host-derived tools never enter persisted enabled lists. Memory follows memory
 * layer switches, runtime tools follow producers, `skill` follows its switch,
 * `tool_search` follows the MCP tool-discovery switch, the plan tools follow the
 * security level, the handoff tools follow the conversation's context, and the
 * two web tools follow the conversation's web-access switch.
 * Normalization and preset application share this rule.
 */
export function isHostDerivedToolName(name: string): boolean {
  return MEMORY_TOOL_NAME_SET.has(name)
    || TASK_RUNTIME_TOOL_NAME_SET.has(name)
    || PLAN_TOOL_NAME_SET.has(name)
    || HANDOFF_TOOL_NAME_SET.has(name)
    || WEB_TOOL_NAME_SET.has(name)
    || name === SKILL_TOOL_NAME
    || name === TOOL_SEARCH_TOOL_NAME;
}
