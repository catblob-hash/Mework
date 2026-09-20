/**
 * Which recorded tool calls the timeline may re-execute with edited arguments.
 *
 * Editing a card's `input` without re-executing is not an option: the host
 * compares `tool_name/requested_input/input/result/subagent` against the stored
 * card and rejects the whole conversation write when they diverge without a
 * fresh attestation (`validate_conversation_tool_cards`). So a tool that cannot
 * be safely re-executed cannot have its arguments edited at all.
 *
 * A name qualifies only when both hold:
 *  - re-running it changes nothing outside this app's own record, so the user
 *    gets a new result rather than a second side effect; and
 *  - `security.rs::classify` accepts it outside the model run loop. Every other
 *    built-in name — plus MCP tools and anything unknown — is rejected there,
 *    so offering a rerun would only produce an error.
 *
 * Shell is deliberately absent even though the host classifies `bash "git
 * status"` as a read: that verdict comes from analyzing the concrete command,
 * which this list cannot do.
 */
const RERUNNABLE_TOOLS: ReadonlySet<string> = new Set([
  "ls",
  "grep",
  "find",
  "read",
  "lsp",
  "preview_list",
  "preview_logs",
  "preview_snapshot",
  "preview_inspect",
  "preview_console_logs",
  "preview_network",
  "preview_screenshot"
]);

export function canRerunTool(toolName: string): boolean {
  return RERUNNABLE_TOOLS.has(toolName);
}
