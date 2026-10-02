`plan` holds the Markdown document you read in the plan pane before implementation starts. `write` replaces the whole document, `read` returns what is stored, and the pane updates as each write lands, so the plan is visible while it is still taking shape. The host supplies it from the first time the conversation's **Plan mode** switch goes on, and keeps it for the rest of the conversation — it is never in the tool picker.

## Approval

Neither action asks, at any level. The document lives in the conversation store rather than on disk: `read` is classified as a read of host state and `write` as a change to this conversation's own state. An `action` the host cannot read is classified as the write. The tool cannot be executed or approved by hand, so no timeline entry can be replayed to move the conversation's plan behind a running turn.

## Behavior and limits

`write` refuses empty content and caps the document at 200,000 characters — a plan, not the code. Every write stores a complete replacement and marks the document a fresh draft, a rewrite after feedback or an approval included; the receipt reports the size saved. Before anything has been written, `read` answers that there is no plan yet. The planning guidance — research, design, review, write the plan, then ask with `exit_plan_mode` — is not in this description: each time the switch goes on, the host appends it to the conversation as a system message (`system.plan_mode` in a [prompt profile](../prompt-profiles.html)). The tool itself works whether or not plan mode is on. A saved plan appears as an **Implementation plan** row in the Tasks pane, and clicking it opens the plan pane beside the conversation. Subagents never get this tool: the plan belongs to the conversation the user is talking to.

## Related

- [Working with Mework](../working.html#plan-mode) — the Plan mode switch.
- [exit_plan_mode](exit_plan_mode.html) — asks you to approve the document written here.
