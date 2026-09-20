/**
 * Names of the six long-term memory tools. They are governed by the conversation's
 * two memory-layer toggles rather than the enabled-tool list: the host injects or
 * removes them per enabled layer when constructing trusted requests. The renderer
 * excludes the complete set from tool selection and persisted enabled-tool lists.
 * This corresponds to Rust `mework_memory::MEMORY_TOOL_NAMES`.
 */
export const MEMORY_TOOL_NAMES = [
  "read_global_memory",
  "read_project_memory",
  "create_global_memory",
  "create_project_memory",
  "edit_global_memory",
  "edit_project_memory"
] as const;

export const MEMORY_TOOL_NAME_SET: ReadonlySet<string> = new Set(MEMORY_TOOL_NAMES);

/** The three tools added when global memory is enabled. Matches Rust `GLOBAL_MEMORY_TOOL_NAMES`. */
export const GLOBAL_MEMORY_TOOL_NAMES = [
  "read_global_memory",
  "create_global_memory",
  "edit_global_memory"
] as const;

/** The three tools added when project memory is enabled. Matches Rust `PROJECT_MEMORY_TOOL_NAMES`. */
export const PROJECT_MEMORY_TOOL_NAMES = [
  "read_project_memory",
  "create_project_memory",
  "edit_project_memory"
] as const;
