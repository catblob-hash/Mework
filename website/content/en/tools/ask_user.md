`ask_user` stops the turn and puts a multiple-choice card above the composer. The model reaches for it when it has hit a fork it should not pick for you — which of two approaches, which of three files is the right one, whether to include a migration — and each option carries a short line saying what choosing it means. Your answer arrives as the next user message, which starts the next turn.

## Approval

No security level asks, and no approval card is involved: the tool is itself the question. It is classified as coordination inside the conversation and can only be scheduled by the model run loop, never executed by hand. Subagents never get it — a child that needs a decision reports back to its parent instead.

## Behavior and limits

One call carries up to four questions, each with two to four options; an **Other** free-text choice is added to every question by the card, so you are never boxed into the offered answers, and a question marked multi-select accepts several. An option may carry preview text, shown while that option is focused or hovered. A valid call ends the turn with no further request to the provider, and any other tool calls the model emitted in the same round are skipped rather than run. The card stays above the composer until answered, with header buttons to edit the questions or delete them. Answering sends an ordinary user message pairing each question with the answer you gave, so the model reads it as your words — and you can ignore the card and type something else, which closes the question and goes to the model as that message instead. An answer given while a run is still settling is held and sent once the conversation is idle.

## Related

- [Working with Mework](../working.html#subagents-workflows-and-tasks) — where this sits among the orchestration tools.
- [exit_plan_mode](exit_plan_mode.html) — the card that asks about a plan.
