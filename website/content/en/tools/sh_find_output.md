`sh_find_output` runs one command line exactly as [sh](sh.html) runs it, through the machine's POSIX `/bin/sh`, and returns only the parts of its output that match the call's `query` at or above its `threshold`, as scored by the TypeSafe Jev decision model. Like `sh`, it is listed only when a machine in the conversation has `sh` (macOS, Linux, WSL). It is the decision-model form of `sh`, chosen in the Shell tool window.

## Approval

As for `sh`: every call raises a card at Manual approval, Accept edits and Plan mode; only Full access — or a `PreToolUse` hook answering `allow` — clears it. The command is read with the same static analysis as `bash`. **Always allow** is not offered, and there is no OS-level sandbox.

## Behavior and limits

The command runs as `sh` runs it — `sh -c` with no startup files, so write portable POSIX shell — and registers on the task page as an `sh` command. What is scored, the status line kept above the report, the stop at `timeout` in place of a move to the background, the absence of `run_in_background`, and the check of `query`, `threshold` and the key only after the command has run all work as they do for [bash_find_output](bash_find_output.html); hits are labelled `output lines N-M`.

## The decision model

In the Shell tool window under **sh**, **Add decision-model parameters** turns on `sh` and `sh_find_output` together, and **Decision model only** leaves `sh_find_output` alone. Seeded presets leave it off. The query and the output's chunks are sent to TypeSafe's API, which needs its key under Global settings → Decision model providers.

## Related

- [sh](sh.html), [bash_find_output](bash_find_output.html), [find_output](find_output.html)
- [Decision-model tools](../tools.html#decision-model)
- [Working with Mework](../working.html#tools-and-approvals)
