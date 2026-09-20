`fork` hands a whole job to a separate conversation that you follow on its own. The call raises a request; you answer it on a card of your own, and an approved fork becomes a child conversation that starts its first run by itself. Use it to delegate something you want to watch separately — never to get an answer back: nothing the child produces returns to the parent.

## Approval

The decision is yours at every security level, Full access included: the tool creates nothing, it only raises a card. The card is non-blocking — the call returns at once with a receipt saying a request was submitted, so the turn continues while the card waits — and it offers two answers, approve and deny, with no **Always allow**. Approved and declined decisions stay in the source conversation's Tasks pane and are never reported to the model.

## Behavior and limits

The prompt may be up to 32,768 characters and is the child's entire instruction: it becomes the child's one user message, with no history, no context and no later correction. The child inherits the parent's settings verbatim — security level, enabled tools, system prompt, memory switches, roles — along with its worktree, run location and extra working directories, so it holds exactly the parent's permissions. Its title is the prompt's first non-empty line, up to 32 characters. Nesting is one level deep: forking a child produces that child's sibling. A conversation may have at most 16 unanswered requests, and a card nobody answers expires after 30 minutes. Requests are forgotten on restart, while the decisions they produced are stored; deleting the source conversation withdraws its open cards. Only the main agent may call it.

## Related

- [Working with Mework](../working.html#tools-and-approvals) — the security levels and the fork column.
- [agent_spawn](agent_spawn.html), [workflow](workflow.html) — use these when you need the result.
