`exit_plan_mode` submits the plan for review. It takes no arguments and carries no content: the document it presents is the one already written with `plan`, and the call is the request to leave Plan mode and start implementing. It is the tool that asks whether to proceed; questions about the approach itself belong earlier, while the plan is being written.

## Approval

The call blocks on a card that is raised whatever the conversation's settings say, and that never offers **Always allow**. The card shows the plan's own first heading, and raising it opens the plan pane beside the conversation, because the answer is about the plan rather than about anything in the timeline. Three answers: **Yes, auto-accept edits** moves the conversation to Accept edits, **Yes, manually approve edits** moves it to Manual — both take effect in the same turn, and the composer shows the new level immediately — and **No, keep planning** opens a feedback box, which must not be empty, and leaves the conversation in Plan mode. Stopping generation releases the card without recording a refusal.

## Behavior and limits

The call fails outside Plan mode, saying that only you can put the conversation into that mode from the composer, and fails when no plan has been written yet, pointing at `plan`. On approval the result hands the approved plan back together with the permission mode now in force, and the document is marked approved. On "keep planning" the call still succeeds — the model asked a question and got an answer — returning your feedback with an instruction to revise and call again; the document is marked rejected, and the next write starts a fresh draft. Subagents never get this tool and cannot leave their parent's mode.

## Related

- [Working with Mework](../working.html#plan-mode) — the mode itself and what it refuses.
- [plan](plan.html) — writes the document this submits.
