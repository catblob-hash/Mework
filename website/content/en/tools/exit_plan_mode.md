`exit_plan_mode` asks you to approve the plan. It takes no arguments and carries no content: the document it presents is the one already written with `plan`. It is the tool that asks whether to proceed; questions about the approach itself belong earlier, while the plan is being written.

## Approval

The call blocks on a card that is raised whatever the conversation's settings say, and that never offers **Always allow**. The card shows the plan's own first heading, and raising it opens the plan pane beside the conversation, because the answer is about the plan rather than about anything in the timeline. It has a reply box and two answers: **Approve**, or **Send feedback** with what you typed in the box, which must not be empty. There is no refusal, and neither answer changes the security level. Stopping generation releases the card without recording an answer.

## Behavior and limits

The call fails when plan mode is off — the tool outlives it — and when no plan has been written yet, pointing at `plan`. On approval the result hands the approved plan back, the document is marked approved and plan mode turns off, so the model implements the plan in the same turn; the tools stay, so turning plan mode on again for a later task goes through the same cycle. On feedback the call still succeeds — the model asked a question and got an answer — returning what you wrote with an instruction to revise the plan and call again; the document is marked as needing changes, and the next write starts a fresh draft. The model may ask as many times as it takes. Subagents never get this tool.

## Related

- [Working with Mework](../working.html#plan-mode) — the Plan mode switch.
- [plan](plan.html) — writes the document this presents.
