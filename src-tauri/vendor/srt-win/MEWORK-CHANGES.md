# srt-win in Mework

This directory is `vendor/srt-win-src` of
[anthropic-experimental/sandbox-runtime](https://github.com/anthropic-experimental/sandbox-runtime)
at commit `ddbeb74` (v0.0.77), licensed under the Apache License 2.0 (`LICENSE`,
Copyright Anthropic, PBC). It is the Windows backend Claude Code's sandbox uses: a
dedicated local account, provisioned once with administrator rights, whose processes
run under a restricted token, in a kill-on-close job on a private desktop, fenced from
the network by Windows Filtering Platform filters keyed on the account's SID.

Mework's Windows agent runs each sandboxed conversation (a *cell*) through
`srt-win exec`. Changes made here, all in service of that:

- `exec --stdin` (`src/cli.rs`, `src/logon.rs`): the broker keeps the runner's
  stdin pipe open after the spec and pumps its own stdin into it, so the sandboxed
  child — a Mework agent speaking its protocol over stdin and stdout — has a
  standard input for as long as it runs. Without the flag, behaviour is unchanged.
- `exec --deny-delete <path>` (`src/cli.rs`): the object-only DELETE deny srt-win
  already applies to placeholder directories, exposed for existing paths, so a
  workspace's `.git` can be pinned (neither removed nor renamed) without denying
  anything inside it.
- `wfp ports` (`src/loopback_ports.rs`, `src/runner.rs`, `src/cli.rs`): the runner,
  as the sandbox user, finds which loopback ports the fence lets it reach, and prints
  them. The install is shared, and another program may have made it — or replaced it
  with `--force` — with its own `--proxy-port-range`, which only an elevated `wfp
  status` can read; Mework runs its proxy on a port this reports instead of assuming
  the default.
- `Cargo.toml`: an empty `[workspace]` table, so the crate builds on its own inside
  Mework's repository.

Nothing else is modified.
