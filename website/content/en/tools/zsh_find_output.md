`zsh_find_output` runs one command line exactly as [zsh](zsh.html) runs it and returns only the parts of its output that match the call's `query` at or above its `threshold`, as scored by the TypeSafe Jev decision model. Like `zsh`, it is listed only when a machine in the conversation has zsh, and each call may name only a workspace whose machine has it. It is the decision-model form of `zsh`, chosen in the Shell tool window.

## Approval

As for `zsh`: every call raises a card at Manual approval, Accept edits and Plan mode; only Full access — or a `PreToolUse` hook answering `allow` — clears it. The command is read with the same static analysis as `bash`, which only sets the card's risk level and rule. **Always allow** is not offered, and there is no OS-level sandbox.

## Behavior and limits

The command runs as `zsh` runs it — a login shell (`zsh -l -c`) on this machine, `zsh -f -c` in WSL and over SSH — and registers on the task page as a `zsh` command. What is scored, the status line kept above the report, the stop at `timeout` in place of a move to the background, the absence of `run_in_background`, and the check of `query`, `threshold` and the key only after the command has run all work as they do for [bash_find_output](bash_find_output.html); hits are labelled `output lines N-M`.

## The decision model

In the Shell tool window under **zsh**, **Add decision-model parameters** turns on `zsh` and `zsh_find_output` together, and **Decision model only** leaves `zsh_find_output` alone. Seeded presets leave it off. The query and the output's chunks are sent to TypeSafe's API, which needs its key under Global settings → Decision model providers.

## Related

- [zsh](zsh.html), [bash_find_output](bash_find_output.html), [find_output](find_output.html)
- [Decision-model tools](../tools.html#decision-model)
- [Working with Mework](../working.html#tools-and-approvals)
