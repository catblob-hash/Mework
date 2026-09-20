`plan` holds the Markdown document you read in the plan pane before implementation starts. `write` replaces the whole document, `read` returns what is stored, and the pane updates as each write lands, so the plan is visible while it is still taking shape. The tool exists only in Plan mode, where the host supplies it automatically — it is never in the tool picker or a preset.

## Approval

Neither action asks, at any level. The document lives in the conversation store rather than on disk, which is why writing it is possible in the very mode that refuses every filesystem write: `read` is classified as a read of host state and `write` as a change to this conversation's own state. An `action` the host cannot read is classified as the write. The tool cannot be executed or approved by hand, so no timeline entry can be replayed to move the conversation's plan behind a running turn.

## Behavior and limits

`write` refuses outside Plan mode, refuses empty content, and caps the document at 200,000 characters — a plan, not the code. Every write stores a complete replacement and marks the document a fresh draft, a rewrite after a rejection included; the receipt reports the size saved. `read` is not gated on the mode, so the approved plan can still be fetched after `exit_plan_mode` has moved the conversation to another level; before anything has been written it answers that there is no plan yet. A saved plan appears as an **Implementation plan** row in the Tasks pane, and clicking it opens the plan pane beside the conversation. Subagents never get this tool: the plan belongs to the conversation the user is talking to.

## Related

- [Working with Mework](../working.html#plan-mode) — what Plan mode allows, refuses and asks about.
- [exit_plan_mode](exit_plan_mode.html) — submits the document written here for review.
