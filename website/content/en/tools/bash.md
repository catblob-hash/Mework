The `bash` tool runs one command line through a native Bash — Git for Windows or MSYS2, never the System32 WSL launcher. The model reaches for it when no dedicated tool covers the job: builds, tests, `git`, package managers.

## Approval

Every call raises a card at Manual approval, Accept edits and Plan mode; only Full access — or a `PreToolUse` hook answering `allow` — clears it. Static analysis only sets the card's risk level and rule: a redirection, a command substitution or an explicit executable path is unbounded, and a command over 10,000 characters is not analysed at all. A recursive delete that could reach the filesystem root, your home directory or a system path, or whose target cannot be resolved statically, is a confirmation no security level and no hook turns off. `bash` never carries a standing allowance, so **Always allow** is not offered. There is no OS-level sandbox: approval lets the command run, it does not confine it.

## Behavior and limits

The conversation's first call runs your rc file into a snapshot every later shell sources, so your aliases and functions are there; a login shell is the fallback when it cannot be built. A directory change carries over only when the command succeeded and stayed inside the workspace. Output is decoded per line as UTF-8, or else the system ANSI code page, CRLF folded to LF; a failed call leads with `Exit code N`. The model sees at most 64 KiB, marked truncated; the task page keeps the last 256 KiB. `timeout` is clamped to 600000 ms and defaults to 120000 ms; on expiry the command is handed to the task surface, killed only when no slot takes it. `run_in_background` returns a `shell:<id>` address at once. In WSL and over SSH it runs `--noprofile --norc`, with no snapshot and no directory carried over.

## Related

- [powershell](powershell.html), [task_wait](task_wait.html), [task_list](task_list.html)
- [Working with Mework](../working.html#tools-and-approvals)
- [Hooks](../hooks.html)
- [bash_find_output](bash_find_output.html), [find_output](find_output.html) — only the parts of a command's output that match a description
