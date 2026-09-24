`powershell_find_output` runs one command exactly as [powershell](powershell.html) runs it — `-NoProfile -NonInteractive -ExecutionPolicy Bypass -Command`, PowerShell 7 preferred over Windows PowerShell 5.1 — and returns only the parts of its output that match the call's `query` at or above its `threshold`, as scored by the TypeSafe Jev decision model. It is offered wherever `powershell` is, a Windows machine with PowerShell, and never for WSL. It is the decision-model form of `powershell`, chosen in the Shell tool window.

## Approval

Exactly as for `powershell`: every call raises a card at Manual approval, Accept edits and Plan mode, showing the command; only Full access — or a `PreToolUse` hook answering `allow` — clears it. Static analysis only sets the card's risk level and rule: file access it can see is judged against the file tools' path policy, anything touching a non-filesystem provider, a UNC path or the call operator is unbounded, and a recursive delete that could reach the filesystem root, your home directory or a system path is a confirmation no security level and no hook turns off. **Always allow** is not offered, and there is no OS-level sandbox.

## Behavior and limits

The command runs on `powershell`'s own path — the same UTF-8 prologue, line-by-line decoding with CRLF folded to LF, directory rules and `timeout` clamp — and registers on the task page as a `powershell` command. On a Windows machine reached over SSH it runs through that machine's agent, starting at the workspace root. What is scored, the status line kept above the report, the stop at `timeout` in place of a move to the background, the absence of `run_in_background`, and the check of `query`, `threshold` and the key only after the command has run all work as they do for [bash_find_output](bash_find_output.html); hits are labelled `output lines N-M`.

## The decision model

In the Shell tool window under **PowerShell**, **Add decision-model parameters** turns on `powershell` and `powershell_find_output` together, and **Decision model only** leaves `powershell_find_output` alone. Seeded presets leave it off. The query and the output's chunks are sent to TypeSafe's API, which needs its key under Global settings → Decision model providers.

## Related

- [powershell](powershell.html), [bash_find_output](bash_find_output.html), [find_output](find_output.html)
- [Decision-model tools](../tools.html#decision-model)
- [Working with Mework](../working.html#tools-and-approvals)
- [Hooks](../hooks.html)
