The `powershell` tool runs one command through PowerShell, launched `-NoProfile -NonInteractive -ExecutionPolicy Bypass -Command`. PowerShell 7 is preferred, Windows PowerShell 5.1 the last resort. The model reaches for it on Windows work Git Bash does not cover: cmdlets, .NET types, the registry.

## Approval

Every shell call raises a card at Manual approval, Accept edits and Plan mode; only Full access — or a `PreToolUse` hook answering `allow` — clears it. Static analysis only sets the card's risk level and rule: file access it can see (`Get-Content`, `Remove-Item` and their aliases) is judged against the file tools' path policy; anything touching a non-filesystem provider, a UNC path or the call operator is unbounded, and a command over 10,000 characters is not analysed at all. A recursive delete that could reach the filesystem root, your home directory or a system path is a confirmation no security level and no hook turns off. `powershell` never carries a standing allowance, so **Always allow** is not offered. There is no OS-level sandbox: approval means the command may run, not that it is confined.

## Behavior and limits

A prologue sets a UTF-8 `Out-File` default, plus `$OutputEncoding` and plain-text rendering under `FullLanguage`; it is skipped when the command must lead syntactically (`param`, `using …`, a leading `[`). The file cmdlets keep their own defaults, so `Get-Content` on a BOM-less UTF-8 file returns mojibake under 5.1. Output is decoded line by line — UTF-8 where valid, the system ANSI code page otherwise — with CRLF folded to LF; a failed call leads with `Exit code N`, and the model sees at most 64 KiB, marked truncated. The final directory carries over only if it is inside the workspace, never from a backgrounded command. `timeout` is clamped to 600000 ms and defaults to 120000 ms. On a Windows machine reached over SSH it runs through that machine's agent with its console output switched to UTF-8, starting at the workspace root every call; it is never offered for WSL or a machine without PowerShell.

## Related

- [bash](bash.html), [read](read.html), [task_wait](task_wait.html)
- [Working with Mework](../working.html#tools-and-approvals)
- [Hooks](../hooks.html)
- [powershell_find_output](powershell_find_output.html) — only the parts of the output that match a description
