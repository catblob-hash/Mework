import { MEMORY_TOOL_NAME_SET } from "./memoryTools";

/**
 * Catalog names for task-runtime tools. They are host-derived, not user settings:
 * conversations that produce tasks must also be able to list and await them, and
 * `box` is the carrier the host folds an unawaited task's result into.
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
  "preview_dialog",
  "preview_find_logs"
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
 * catalog is covered the day it lands: the settings gather all of them behind
 * the one preview row, and none of them is a call the timeline offers to place.
 */
export function isPreviewToolName(name: string): boolean {
  return name.startsWith("preview_");
}

/**
 * Catalog names of the tools that run through the decision model. Mirrors Rust
 * `decision_tools::DECISION_TOOL_NAMES`.
 *
 * Each needs the TypeSafe key before it can do anything, so none is on in a
 * seeded preset. `preview_find_logs` still counts as a preview tool everywhere
 * else: the preview settings offer it under `preview_logs`, and the timeline
 * does not offer to re-place a call against a page that is gone.
 */
const DECISION_TOOL_NAMES = [
  "find_content",
  "find_files",
  "find_output",
  "bash_find_output",
  "zsh_find_output",
  "sh_find_output",
  "powershell_find_output",
  "preview_find_logs"
] as const;

const DECISION_TOOL_NAME_SET: ReadonlySet<string> = new Set(DECISION_TOOL_NAMES);

export function isDecisionToolName(name: string): boolean {
  return DECISION_TOOL_NAME_SET.has(name);
}

/**
 * The tools whose decision-model form is a separate catalog tool, each with
 * that tool. The form is read off which of the two are enabled: the base alone
 * is the direct form, both are `augment`, and the variant alone is `replace`.
 * The settings draw each pair as the base tool's row.
 */
export const DECISION_VARIANT_TOOLS = {
  preview_logs: "preview_find_logs",
  bash: "bash_find_output",
  zsh: "zsh_find_output",
  sh: "sh_find_output",
  powershell: "powershell_find_output"
} as const satisfies Record<string, string>;

/** The variant tool that is `name`'s decision-model form, or `null` when it has none. */
export function decisionVariantOf(name: string): string | null {
  return Object.hasOwn(DECISION_VARIANT_TOOLS, name)
    ? DECISION_VARIANT_TOOLS[name as keyof typeof DECISION_VARIANT_TOOLS]
    : null;
}

/**
 * The tools whose decision-model form is parameters of their own rather than a
 * separate tool. Mirrors Rust `decision_tools::DECISION_PARAMETER_TOOLS`. The
 * console and snapshot tools score, so they take `query` and `threshold`; the
 * element tools (click, fill, inspect) have the model choose one element or
 * "none of the above", so they take `query` alone.
 *
 * A conversation decides per tool, in `decisionParameterModes`, whether the
 * pair joins the tool's direct parameters (`augment`) or takes their place
 * (`replace`); a tool with no entry keeps its direct parameters only.
 */
export const DECISION_PARAMETER_TOOL_NAMES = [
  "preview_console_logs",
  "preview_snapshot",
  "preview_inspect",
  "preview_click",
  "preview_fill"
] as const;

const DECISION_PARAMETER_TOOL_NAME_SET: ReadonlySet<string> = new Set(DECISION_PARAMETER_TOOL_NAMES);

export function takesDecisionParameters(name: string): boolean {
  return DECISION_PARAMETER_TOOL_NAME_SET.has(name);
}

/**
 * The decision-parameter tools whose form chooses an element and so can miss:
 * the ones a conversation may ask, in `decisionMissScoring`, to score every
 * element line after a "none of the above". Mirrors Rust
 * `decision_tools::MISS_SCORING_TOOLS`.
 */
export const MISS_SCORING_TOOL_NAMES = [
  "preview_inspect",
  "preview_click",
  "preview_fill"
] as const;

const MISS_SCORING_TOOL_NAME_SET: ReadonlySet<string> = new Set(MISS_SCORING_TOOL_NAMES);

export function scoresMisses(name: string): boolean {
  return MISS_SCORING_TOOL_NAME_SET.has(name);
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
 * conversation's security level — plan mode offers `plan` and `exit_plan_mode`,
 * every other level offers neither — so neither is a user setting. Entering
 * plan mode is the user's own choice in the composer, never a tool call.
 * Mirrors the Rust plan tool names.
 */
const PLAN_TOOL_NAMES = ["plan", "exit_plan_mode"] as const;

const PLAN_TOOL_NAME_SET: ReadonlySet<string> = new Set(PLAN_TOOL_NAMES);

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
 * security level, and the two web tools follow the conversation's web-access
 * switch.
 * Normalization and preset application share this rule.
 */
export function isHostDerivedToolName(name: string): boolean {
  return MEMORY_TOOL_NAME_SET.has(name)
    || TASK_RUNTIME_TOOL_NAME_SET.has(name)
    || PLAN_TOOL_NAME_SET.has(name)
    || WEB_TOOL_NAME_SET.has(name)
    || name === SKILL_TOOL_NAME
    || name === TOOL_SEARCH_TOOL_NAME;
}
