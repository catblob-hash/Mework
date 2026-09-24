The `zsh` tool runs one command line through zsh. It is listed only when a machine in the conversation has zsh — macOS, Linux or a WSL distribution; Mework does not probe for zsh on Windows — and each call may name only a workspace whose machine has it.

## Approval

Every call raises a card at Manual approval, Accept edits and Plan mode; only Full access — or a `PreToolUse` hook answering `allow` — clears it. The command is read with the same static analysis as `bash`, which only sets the card's risk level and rule. `zsh` never carries a standing allowance, so **Always allow** is not offered. There is no OS-level sandbox: approval lets the command run, it does not confine it.

## Behavior and limits

On this machine zsh runs as a login shell (`zsh -l -c`), so `.zprofile` sets up `PATH`; there is no rc snapshot, so interactive-only aliases from `.zshrc` are absent. A directory change carries over only when the command succeeded. In WSL and over SSH it runs `zsh -f -c`, reading no startup files, with no directory carried over. Output, `timeout` and `run_in_background` behave as they do for `bash`.

## Related

- [bash](bash.html), [sh](sh.html), [powershell](powershell.html)
- [Working with Mework](../working.html#tools-and-approvals)
- [zsh_find_output](zsh_find_output.html) — only the parts of the output that match a description
