The `sh` tool runs one command line through the machine's POSIX `/bin/sh` — often dash or BusyBox ash, sometimes bash in POSIX mode. It is listed only when a machine in the conversation has `sh` (macOS, Linux, WSL), and it is what keeps a machine without bash usable.

## Approval

Every call raises a card at Manual approval and Accept edits; only Full access — or a `PreToolUse` hook answering `allow` — clears it. The command is read with the same static analysis as `bash`. `sh` never carries a standing allowance, so **Always allow** is not offered. There is no OS-level sandbox.

## Behavior and limits

It runs `sh -c` with no startup files. On this machine a directory change carries over when the command succeeded; in WSL and over SSH every call starts at its workspace root. Write portable POSIX shell: no `[[ ]]`, arrays, `$'…'` or brace expansion. Output, `timeout` and `run_in_background` behave as they do for `bash`.

## Related

- [bash](bash.html), [zsh](zsh.html)
- [Working with Mework](../working.html#tools-and-approvals)
