import { MEMORY_TOOL_NAME_SET } from "./memoryTools";

/**
 * Catalog names for task-runtime tools. They are host-derived, not user settings:
 * conversations that produce tasks must also be able to list and await them.
 * Keep descriptors for timeline rendering, but exclude them from selection and
 * persisted enabled lists. Mirrors Rust `agents::TASK_RUNTIME_TOOL_NAMES`.
 */
export const TASK_RUNTIME_TOOL_NAMES = ["task_wait", "task_list"] as const;

export const TASK_RUNTIME_TOOL_NAME_SET: ReadonlySet<string> = new Set(TASK_RUNTIME_TOOL_NAMES);

/**
 * Enabling any producer can create a task-list row, making the runtime tools reachable.
 *
 * Mirrors Rust `orchestration::TaskRef`: agents and workflows use the agent pool,
 * Playwright owns a browser session, and shells register shell tasks. The fourth
 * `TaskRef` address, `Terminal`, is deliberately absent: terminals are opened
 * manually, never produced by a tool. `web_search`
 * and `web_fetch` are ordinary asynchronous tools whose results stay on their calls.
 */
export const TASK_PRODUCING_TOOL_NAMES = [
  "agent_spawn",
  "workflow",
  "playwright",
  "bash",
  "powershell"
] as const;

export const TASK_PRODUCING_TOOL_NAME_SET: ReadonlySet<string> = new Set(TASK_PRODUCING_TOOL_NAMES);

/**
 * Catalog name for the on-demand skill tool. It is host-derived from
 * `skillToolEnabled` and the number of resolved skills.
 */
export const SKILL_TOOL_NAME = "skill";

/**
 * Catalog names for the plan-mode tools. The host derives them from the
 * conversation's security level — plan mode offers `plan` and `exit_plan_mode`,
 * every other level offers `enter_plan_mode` — so none of them is a user
 * setting. Mirrors the Rust plan tool names.
 */
export const PLAN_TOOL_NAMES = ["plan", "exit_plan_mode", "enter_plan_mode"] as const;

export const PLAN_TOOL_NAME_SET: ReadonlySet<string> = new Set(PLAN_TOOL_NAMES);

export function isTaskRuntimeToolName(name: string): boolean {
  return TASK_RUNTIME_TOOL_NAME_SET.has(name);
}

/**
 * Host-derived tools never enter persisted enabled lists. Memory follows memory
 * layer switches, runtime tools follow producers, `skill` follows its switch,
 * and the plan tools follow the security level.
 * Normalization and preset application share this rule.
 */
export function isHostDerivedToolName(name: string): boolean {
  return MEMORY_TOOL_NAME_SET.has(name)
    || TASK_RUNTIME_TOOL_NAME_SET.has(name)
    || PLAN_TOOL_NAME_SET.has(name)
    || name === SKILL_TOOL_NAME;
}
