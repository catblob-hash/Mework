`bash_find_output` runs one command line exactly as [bash](bash.html) runs it, then hands the output to the TypeSafe Jev decision model instead of to the conversation: only the parts that match the call's `query` at or above its `threshold` come back. The model reaches for it when it knows before running a command that it wants one thing out of a long output — why the tests failed, where the build broke, which package pulled in a version — and not the rest. It is the decision-model form of `bash`, chosen in the Shell tool window.

## Approval

Exactly as for `bash`: every call raises a card at Manual approval, Accept edits and Plan mode, showing the command; only Full access — or a `PreToolUse` hook answering `allow` — clears it. The command goes through the same static analysis, which only sets the card's risk level and rule, and meets the same recursive-delete confirmation no security level and no hook turns off. A hook sees the call under its own name, so a matcher written `^bash$` does not cover it. **Always allow** is not offered, and there is no OS-level sandbox. Sending the output to TypeSafe is not a separate approval: enabling the tool and saving the key is the opt-in.

## Behavior and limits

The command runs on `bash`'s own path — the same shell, rc snapshot and directory rules, the same `timeout` clamp (default 120000 ms, at most 600000 ms) — and registers on the task page as a `bash` command, which keeps its output where [find_output](find_output.html) can search it again. Two things differ: there is no `run_in_background`, and a command that reaches its `timeout` is stopped rather than moved to the background, the timeout notice becoming its status line.

What is scored is the output as `bash` captures it for the model: the first 64 KiB of standard output and of standard error — stdout first after a success, stderr first after a failure — ending in a truncation line when either was cut. A failed call's status line — `Exit code N`, the stop notice or the timeout notice — is kept verbatim above the report and never sent; a successful call has none, so line 1 is the output's first line. After a formatter-like command, the note naming files you had read that it changed follows the report instead of being scored. A command that fails without printing anything comes back as its status line and `(no output)`, with no request made; one that succeeds silently has only Mework's own completion line to score. The call succeeds or fails with the command, whatever cleared the threshold.

Scoring is the pass [find_output](find_output.html) runs: the output cut into log records, each record scored on its own and several to a request, eight requests at a time under Mework's shared TypeSafe pace. The answer is a count — `1 hit at or above 0.600 for query "why the run failed" (40 records of the command output scored, 2 requests).` — then every record that cleared, highest score first, as `--- output line 31 (score 1.000) ---` followed by the record with its line numbers, or the three best records with their scores when nothing clears.

`query`, `threshold` and the key are only checked after the command has run. A missing argument, a missing or rejected key, or every request failing fails the call with the command's effects already done; its output is then only on its task page.

## The decision model

In the conversation's **Shell** tool window, once **Bash** is on, **Add decision-model parameters** turns on `bash` and `bash_find_output` together, so the model picks per call; **Decision model only** leaves `bash_find_output` alone, and the whole output and background runs go with plain `bash`. A tool a run has already shown the model stays on for the rest of the conversation, so once either has gone out, only a form that keeps it can be chosen. Seeded presets leave `bash_find_output` off, because it cannot run until TypeSafe's key is saved under Global settings → Decision model providers. Each request sends TypeSafe's API the query and, for each of its records, the record's label (`output` and its line range) and its text; the command and its status line stay on the machine.

## Related

- [bash](bash.html) — the whole output, or a background run
- [find_output](find_output.html) — search a command's output after it ran
- [task_list](task_list.html) — the `shell:<id>` row the command leaves
- [Decision-model tools](../tools.html#decision-model) — what these tools have in common
- [Working with Mework](../working.html#tools-and-approvals) — the security-level matrix
- [Hooks](../hooks.html)
