`ask_user` puts a multiple-choice card above the composer and waits on it. The model reaches for it when it has hit a fork it should not pick for you — which of two approaches, which of three files is the right one, whether to include a migration — and each option carries a short line saying what choosing it means. The call blocks until you answer, and your answer comes back as the result of that same call, so the model carries on inside the same turn.

## Approval

No security level asks, and no approval card is involved: the tool is itself the question. It is classified as coordination inside the conversation and can only be scheduled by the model run loop, never executed by hand. Subagents never get it — a child that needs a decision reports back to its parent instead.

## Behavior and limits

The card follows Claude Code's AskUserQuestion. One call carries up to four questions, each with two to four options and a tab of its own; an **Other** row lets you type your own answer, and a question marked multi-select accepts several. Picking an option in a single-select question moves on to the next one, and a card with a single single-select question is answered the moment you pick. Cards with more questions end on a review tab where you submit what you answered — unanswered questions may stay blank. Options with preview text switch the question to a side-by-side layout with the preview on the right and a notes line below it (press `n`). **Chat about this** declines the questions, hands back whatever you had answered, and asks the model to talk them over with you.

Three ways out do not answer the card:

- **Closing it** — the close button, `Esc`, or Cancel on the review tab — returns a result saying you closed the card, and the turn ends there without asking the model anything more.
- **Sending a message from the composer** while the card is up hands back whatever you had filled in (or that you closed the card, if nothing), and your message follows that result, so the model reads both in the same turn.
- **Stopping the run** retracts the card along with the run.

Answers are recorded with the call, keyed by question text, and the timeline card shows them afterwards.

## Related

- [Working with Mework](../working.html#subagents-workflows-and-tasks) — where this sits among the orchestration tools.
- [exit_plan_mode](exit_plan_mode.html) — the card that asks about a plan.
